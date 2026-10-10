#!/usr/bin/env python3
"""Summarize exported Core + Bridge JSON logs without reading conversation content."""
import argparse
import collections
import json
import math
import sys

REPORTS = {"VOICE_TURN_LATENCY_REPORT", "VOICE_TURN_REPLY", "VOICE_NATIVE_TURN_LIFECYCLE"}
METRICS = (
    "turn_to_first_ws_audio_ms", "transcript_to_answer_enqueue_ms", "speech_end_to_answer_enqueue_ms",
    "speech_to_final_ms", "dispatch_wait_ms", "filler_decision_ms", "core_headers_ms",
    "core_first_text_ms", "core_first_delta_ms", "model_first_text_ms", "text_to_sentence_ms",
    "first_tts_ttfb_ms", "response_total_ms", "turn_total_ms", "max_output_queue_ms",
    "max_queue_ms", "max_write_ms", "max_ws_write_ms", "max_frame_gap_ms", "webhook_to_start_ms",
    "client_prepare_ms", "send_to_headers_ms", "server_prepare_ms", "server_context_ms", "transport_and_client_overhead_ms",
    "agent_select_ms", "opening_wait_ms", "redis_ms", "database_ms", "refresh_ms", "provider_build_ms",
    "name_ms", "conversation_ms", "context_ms", "projection_ms", "history_and_name_ms",
)

def normalize(raw):
    if not isinstance(raw, dict):
        return None
    # Railway exports can wrap the original structured event in message.
    message = raw.get("message")
    if isinstance(message, str) and message.lstrip().startswith("{"):
        try:
            inner = json.loads(message)
        except json.JSONDecodeError:
            return None
        if isinstance(inner, dict):
            raw = {**raw, **inner}
    event = {}
    for span in raw.get("spans", []):
        if isinstance(span, dict):
            event.update(span)
    if isinstance(raw.get("span"), dict):
        event.update(raw["span"])
    event.update({k: v for k, v in raw.items() if k not in ("span", "spans", "fields")})
    if isinstance(raw.get("fields"), dict):
        event.update(raw["fields"])
    event["event"] = event.get("message", "")
    event["conversation_id"] = event.get("conversation_id") or event.get("external_conversation_id") or event.get("session_id")
    # Existing tracing debug Option<String> fields are rendered as strings.
    turn = event.get("turn_id")
    if isinstance(turn, str) and turn.startswith('Some("') and turn.endswith('")'):
        event["turn_id"] = turn[6:-2]
    return event if isinstance(event["event"], str) and event["event"].startswith(("VOICE_", "CORE_")) else None

def percentile(values, fraction):
    values = sorted(values)
    if not values:
        return None
    position = (len(values) - 1) * fraction
    lower = math.floor(position)
    upper = math.ceil(position)
    return round(values[lower] + (values[upper] - values[lower]) * (position - lower), 1)

def summarize(events):
    sessions = {}
    for event in events:
        if event.get("session_id") and event.get("conversation_id") and event["conversation_id"] != event["session_id"]:
            sessions.setdefault(event["session_id"], event["conversation_id"])
    generations = {}
    for event in events:
        if event.get("event") == "VOICE_TURN_LATENCY_REPORT":
            generations[(event.get("conversation_id"), event.get("generation"))] = (event.get("turn_id") or f'number:{event.get("turn")}', event.get("revision"))
    calls = collections.defaultdict(lambda: {"turns": {}, "events": collections.Counter(), "metrics": collections.defaultdict(list), "session": {}})
    seen = set()
    uncorrelated = 0
    for event in events:
        # Deduplicate overlapping exports only when timestamps are present.
        if event.get("timestamp"):
            fingerprint = json.dumps(event, sort_keys=True)
            if fingerprint in seen:
                continue
            seen.add(fingerprint)
        call_id = event.get("conversation_id")
        if call_id == event.get("session_id") and call_id in sessions:
            call_id = sessions[call_id]
        if not call_id:
            uncorrelated += 1
            continue
        call = calls[call_id]
        call["events"][event["event"]] += 1
        if event["event"] in {"VOICE_SESSION_SUMMARY", "VOICE_SESSION_CLOSE"}:
            call["session"].update({key: event[key] for key in ("session_ms", "outcome", "input_frames", "input_bytes", "partials", "finals", "backchannels", "interruptions", "stt_errors", "stt_reconnects") if key in event})
        turn_id, revision = event.get("turn_id"), event.get("revision")
        if not turn_id and (call_id, event.get("generation")) in generations:
            turn_id, revision = generations[(call_id, event.get("generation"))]
        attempt_id = f"{turn_id}@{revision}" if revision is not None else turn_id
        target = call["metrics"]
        if turn_id:
            turn = call["turns"].setdefault(attempt_id, {"turn_id": turn_id, "attempt_id": attempt_id, "metrics": {}, "events": collections.Counter(), "tools": [], "model_rounds": [], "tts_sentences": [], "context": {}})
            turn["events"][event["event"]] += 1
            target = turn["metrics"]
            for key in ("kind", "turn", "revision", "generation"):
                if event.get(key) is not None:
                    turn[key] = event[key]
            if event["event"] == "VOICE_TTS_SENTENCE":
                turn["tts_sentences"].append({key: event.get(key) for key in ("audio_kind", "outcome", "headers_ms", "first_audio_ms", "max_audio_gap_ms", "elapsed_ms", "tts_total_ms", "tts_connect_ms", "tts_first_chunk_ms", "tts_max_chunk_gap_ms", "max_chunk_gap_ms", "audio_bytes", "audio_frames") if key in event})
            if event["event"] == "CORE_AGENT_PREPARATION":
                turn["context"].update({key: event[key] for key in ("model", "agent", "history_messages", "history_bytes", "context_bytes") if key in event})
            if event["event"] == "CORE_MODEL_ROUND_FINISHED":
                turn["model_rounds"].append({key: event.get(key) for key in ("model_round", "usage_known", "input_tokens", "output_tokens", "reasoning_tokens", "cached_input_tokens", "finish_reason")})
            if event["event"] in REPORTS:
                turn["outcome"] = event.get("outcome", "unknown")
            if event["event"] == "CORE_TOOL_FINISHED":
                turn["tools"].append({key: event.get(key) for key in ("execution_id", "tool", "outcome", "elapsed_ms")})
        # Filler/apology must never count as answer latency.
        for metric in METRICS:
            value = event.get(metric)
            if isinstance(value, (float, int)) and not isinstance(value, bool):
                if metric == "turn_to_first_ws_audio_ms" and event.get("audio_kind", "answer") != "answer":
                    continue
                if turn_id:
                    target[metric] = value
                else:
                    target[metric].append(value)
    populations = {"greeting": collections.defaultdict(list), "turn": collections.defaultdict(list)}
    outcomes = collections.Counter()
    turns = []
    for call_id, call in calls.items():
        for turn in call["turns"].values():
            turn["conversation_id"] = call_id
            outcomes[turn.get("outcome", "unreported")] += 1
            # Completed and incomplete populations must not be silently combined.
            if turn.get("outcome") == "completed":
                for key, value in turn["metrics"].items():
                    populations["greeting" if turn.get("kind") == "greeting" else "turn"][key].append(value)
            turns.append(turn)
    percentiles = {kind: {key: {"n": len(values), "p50_ms": percentile(values, .5), "p95_ms": percentile(values, .95)}
                    for key, values in metrics.items()} for kind, metrics in populations.items()}
    slow = sorted(turns, key=lambda turn: turn["metrics"].get("turn_to_first_ws_audio_ms", turn["metrics"].get("transcript_to_answer_enqueue_ms", -1)), reverse=True)
    return {"call_count": len(calls), "outcomes": dict(outcomes), "uncorrelated_events": uncorrelated,
            "percentiles_completed_only": percentiles, "calls": dict(calls), "slow_turns": slow}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="+", help="Core/Bridge JSONL exports; - for stdin")
    parser.add_argument("--json", action="store_true", help="Machine-readable output, including all turns")
    args = parser.parse_args()
    events = []
    skipped = 0
    for path in args.paths:
        source = sys.stdin if path == "-" else open(path, encoding="utf-8")
        try:
            for line in source:
                try:
                    event = normalize(json.loads(line))
                except json.JSONDecodeError:
                    skipped += 1
                    continue
                if event:
                    events.append(event)
        finally:
            if source is not sys.stdin:
                source.close()
    result = summarize(events)
    result["invalid_json_lines"] = skipped
    if args.json:
        print(json.dumps(result, indent=2))
        return
    print(f'Calls: {result["call_count"]}; outcomes: {result["outcomes"]}; uncorrelated: {result["uncorrelated_events"]}; invalid JSON: {skipped}')
    for kind, metrics in result["percentiles_completed_only"].items():
        print(f'\n{kind}: completed turns only (milliseconds)')
        for metric, stats in metrics.items():
            print(f'  {metric:42} n={stats["n"]:<4} p50={stats["p50_ms"]:<8} p95={stats["p95_ms"]}')
    print('\nSlow turns (missing latency remains unknown):')
    for turn in result["slow_turns"][:20]:
        print(f'  {turn["conversation_id"]}/{turn["turn_id"]}: {turn.get("kind", "unknown")} {turn.get("outcome", "unreported")} {turn["metrics"]} tools={turn["tools"]}')

if __name__ == "__main__":
    main()
