import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("analysis", Path(__file__).with_name("analyze-voice-logs.py"))
analysis = importlib.util.module_from_spec(spec)
spec.loader.exec_module(analysis)

class AnalysisTests(unittest.TestCase):
    def test_out_of_order_join_and_failure_population(self):
        events = [
            {"event": "VOICE_TRANSPORT_FIRST_AUDIO", "conversation_id": "call", "generation": 1, "audio_kind": "answer", "turn_to_first_ws_audio_ms": 1200},
            {"event": "VOICE_TRANSPORT_FIRST_AUDIO", "conversation_id": "call", "generation": 1, "audio_kind": "filler", "turn_to_first_ws_audio_ms": 50},
            {"event": "VOICE_TURN_LATENCY_REPORT", "conversation_id": "call", "generation": 1, "turn_id": "t1", "outcome": "completed", "kind": "turn"},
            {"event": "VOICE_TURN_LATENCY_REPORT", "conversation_id": "call", "generation": 2, "turn_id": "t2", "outcome": "cancelled", "kind": "turn", "core_first_text_ms": None},
        ]
        result = analysis.summarize(events)
        self.assertEqual(result["outcomes"], {"completed": 1, "cancelled": 1})
        self.assertEqual(result["percentiles_completed_only"]["turn"]["turn_to_first_ws_audio_ms"]["p95_ms"], 1200)
        self.assertNotIn("core_first_text_ms", result["calls"]["call"]["turns"]["t2"]["metrics"])
    def test_wrapped_and_nested_json(self):
        import json
        inner = {"fields": {"message": "CORE_TOOL_FINISHED", "elapsed_ms": 200}, "spans": [{"conversation_id": "call", "turn_id": "t"}]}
        event = analysis.normalize({"message": json.dumps(inner)})
        self.assertEqual(event["conversation_id"], "call")
        self.assertEqual(event["turn_id"], "t")
        self.assertEqual(event["event"], "CORE_TOOL_FINISHED")
    def test_revisions_and_context_remain_separate(self):
        events = [
            {"event": "VOICE_TURN_LATENCY_REPORT", "conversation_id": "c", "turn_id": "t", "revision": 1, "outcome": "cancelled"},
            {"event": "VOICE_TURN_LATENCY_REPORT", "conversation_id": "c", "turn_id": "t", "revision": 2, "outcome": "completed"},
            {"event": "CORE_AGENT_PREPARATION", "conversation_id": "c", "turn_id": "t", "revision": 2, "history_bytes": 5000},
        ]
        result = analysis.summarize(events)
        self.assertEqual(len(result["calls"]["c"]["turns"]), 2)
        self.assertEqual(result["calls"]["c"]["turns"]["t@2"]["context"]["history_bytes"], 5000)
        self.assertEqual(result["outcomes"]["cancelled"], 1)
    def test_native_socket_and_turn_share_a_call(self):
        events = [
            {"event": "VOICE_SESSION_OPEN", "session_id": "s", "conversation_id": "s"},
            {"event": "VOICE_NATIVE_TURN_LIFECYCLE", "session_id": "s", "conversation_id": "c", "turn_id": "t", "kind": "greeting", "outcome": "completed", "turn_to_first_ws_audio_ms": 800},
        ]
        result = analysis.summarize(events)
        self.assertEqual(result["call_count"], 1)
        self.assertEqual(result["percentiles_completed_only"]["greeting"]["turn_to_first_ws_audio_ms"]["p50_ms"], 800)
    def test_missing_percentile_stays_unknown(self):
        self.assertIsNone(analysis.percentile([], .95))
        self.assertEqual(analysis.percentile([100, 300], .5), 200)

if __name__ == "__main__":
    unittest.main()
