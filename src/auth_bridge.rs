/**
* Branded OAuth handoff page. Google/Supabase redirect here (a real HTTPS
* URL, unlike vox://) so the browser can render Vox's own branding before
* forwarding the auth code to the desktop app via the vox:// custom scheme.
* Mirrors the page vox-desktop's dev build serves from its local loopback
* listener (src-tauri/src/auth.rs) — same look, but hosted publicly since
* release builds have no local server for Supabase to redirect to.
*/
use axum::response::{Html, IntoResponse};

const AUTH_BRIDGE_HTML: &str = concat!(
    r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Vox is ready</title>
  <link rel="icon" type="image/svg+xml" href="data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 64 64'><rect width='64' height='64' rx='18' fill='%23101317'/><circle cx='32' cy='32' r='14' fill='%23ff6363'/></svg>">
  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link href="https://fonts.googleapis.com/css2?family=Geist+Mono:wght@400;500&family=Inter:wght@400;500;600&display=swap" rel="stylesheet">
  <style>
    :root {
      /* Raycast / Vox Palette Tokens */
      --color-void-black: #040506;
      --color-ink: #07080a;
      --color-obsidian: #111214;
      --color-smoke: #6a6b6c;
      --color-ash: #9c9c9d;
      --color-mist: #e6e6e6;
      --color-pure-white: #ffffff;
      --color-coral-pulse: #ff6363;

      /* Web Fonts */
      --font-inter: 'Inter', ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      --font-geistmono: 'Geist Mono', 'SF Mono', Menlo, Monaco, Consolas, monospace;
    }

    * {
      box-sizing: border-box;
      margin: 0;
      padding: 0;
    }

    body {
      background-color: var(--color-void-black);
      color: var(--color-pure-white);
      font-family: var(--font-inter);
      min-height: 100vh;
      display: flex;
      flex-direction: column;
      align-items: center;
      justify-content: center;
      padding: 24px;
      position: relative;
      overflow: hidden;
      -webkit-font-smoothing: antialiased;
      -moz-osx-font-smoothing: grayscale;
    }

    /* Ambient Hero Glow */
    .ambient-glow {
      position: absolute;
      inset: 0;
      pointer-events: none;
      z-index: 0;
      background:
        radial-gradient(ellipse 650px 340px at 50% 40%, rgba(255, 99, 99, 0.09) 0%, rgba(20, 60, 163, 0.04) 42%, transparent 70%),
        radial-gradient(circle 800px at 50% 50%, rgba(4, 5, 6, 0.4) 0%, #040506 100%);
    }

    /* Full-bleed film grain, same generator vox-desktop uses on its widgets
       and sign-in screen. */
    .page-noise {
      position: absolute;
      inset: 0;
      pointer-events: none;
      z-index: 0;
      opacity: 0.12;
      mix-blend-mode: soft-light;
      background-image: url("data:image/svg+xml,%3Csvg viewBox='0 0 200 200' xmlns='http://www.w3.org/2000/svg'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.85' numOctaves='4' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)'/%3E%3C/svg%3E");
      background-size: 160px 160px;
    }

    /* Main layout (open, no card container) */
    .main-wrap {
      position: relative;
      z-index: 1;
      display: flex;
      flex-direction: column;
      align-items: center;
      text-align: center;
      max-width: 520px;
      margin-top: -36px;
      animation: fadeUp 0.7s cubic-bezier(0.16, 1, 0.3, 1) both;
    }

    /* Vox Logo (pure orb, no container) */
    .vox-logo {
      width: 64px;
      height: 64px;
      display: block;
      margin-bottom: 24px;
      transition: transform 0.3s cubic-bezier(0.16, 1, 0.3, 1);
    }

    .vox-logo:hover {
      transform: scale(1.06);
    }

    /* Elegant h1 matching web */
    h1 {
      font-family: var(--font-inter);
      font-size: clamp(2.2rem, 4.5vw, 3rem);
      font-weight: 500;
      color: var(--color-pure-white);
      letter-spacing: -0.02em;
      line-height: 1.15;
      margin-bottom: 12px;
      text-wrap: balance;
    }

    /* Subtitle description */
    .description {
      font-size: 15px;
      line-height: 1.55;
      color: var(--color-ash);
      font-weight: 400;
      max-width: 380px;
      text-wrap: balance;
    }

    /* Footer metadata pinned to bottom center */
    .footer-meta {
      position: fixed;
      bottom: 28px;
      left: 50%;
      transform: translateX(-50%);
      display: flex;
      align-items: center;
      justify-content: center;
      gap: 10px;
      font-family: var(--font-geistmono);
      font-size: 11px;
      letter-spacing: 0.06em;
      color: var(--color-smoke);
      white-space: nowrap;
      z-index: 1;
      user-select: none;
      opacity: 0.85;
      transition: opacity 0.2s ease, color 0.2s ease;
    }

    .footer-meta:hover {
      opacity: 1;
      color: var(--color-ash);
    }

    .footer-meta .sep {
      color: #2b2c2e;
    }

    .footer-meta .dot {
      width: 5px;
      height: 5px;
      border-radius: 50%;
      background: var(--color-coral-pulse);
      display: inline-block;
      box-shadow: 0 0 6px var(--color-coral-pulse);
    }

    @keyframes fadeUp {
      from {
        opacity: 0;
        transform: translateY(12px) scale(0.98);
      }
      to {
        opacity: 1;
        transform: translateY(0) scale(1);
      }
    }
  </style>
</head>
<body>
  <div class="ambient-glow"></div>
  <div class="page-noise"></div>

  <div class="main-wrap">
    <canvas class="vox-logo" id="vox-orb" width="64" height="64" role="img" aria-label="Vox"></canvas>

    <h1 id="auth-title">Vox is ready</h1>
    <p class="description" id="auth-description">Opening Vox…</p>
  </div>

  <div class="footer-meta">
    <span class="dot"></span>
    <span>VOX BRIDGE</span>
    <span class="sep">|</span>
    <span>READY</span>
  </div>

  <script>
"##,
    include_str!("../assets/thinking-orb-engine.js"),
    r##"

    (function () {
      var canvas = document.getElementById('vox-orb');
      if (!canvas) return;
      var displaySize = 64;
      var engineSize = 64;
      var dark = true;
      var dpr = Math.min(3, window.devicePixelRatio || 1);
      var pixel = Math.round(displaySize * dpr);
      canvas.width = pixel;
      canvas.height = pixel;
      var ctx = canvas.getContext('2d', { alpha: true });
      if (!ctx) return;

      var state = 'composing';
      window.__voxOrbSetState = function (next) { state = next; };

      function paint(t) {
        var resolved = resolvePreset(state, engineSize);
        var draw = MODE_DRAWS[resolved.mode];
        var scale = (displaySize / engineSize) * dpr;
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        ctx.clearRect(0, 0, pixel, pixel);
        ctx.setTransform(scale, 0, 0, scale, 0, 0);
        draw(ctx, engineSize, t, dark, resolved.opts);
      }

      var reduceMotion = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
      if (reduceMotion) {
        paint(0.6);
        return;
      }

      var last = performance.now();
      var sim = 0;
      function tick(now) {
        var dt = Math.min(0.1, (now - last) / 1000);
        last = now;
        sim += dt * resolvePreset(state, engineSize).speed;
        paint(sim);
        requestAnimationFrame(tick);
      }
      paint(0);
      requestAnimationFrame(tick);
    })();
  </script>

  <script>
    const params = new URLSearchParams(window.location.search);
    if (params.has('error')) {
      const title = document.getElementById('auth-title');
      const desc = document.getElementById('auth-description');
      if (title) title.textContent = 'Sign-in cancelled';
      if (desc) desc.textContent = params.get('error_description') || 'Authentication was not completed. You can return to Vox and try again.';
      if (window.__voxOrbSetState) window.__voxOrbSetState('shaping');
    } else {
      // Hand off to the desktop app with the same query string Supabase gave us.
      window.location.replace('vox://auth/callback' + window.location.search);
      setTimeout(function() {
        try { window.close(); } catch (e) {}
      }, 2500);
    }

    window.addEventListener('keydown', function(e) {
      if (e.key === 'Escape') {
        try { window.close(); } catch (e) {}
      }
    });
  </script>
</body>
</html>
"##
);

pub async fn auth_bridge_handler() -> impl IntoResponse {
    Html(AUTH_BRIDGE_HTML)
}
