"use client";

import { useState } from "react";
import { WaveLine } from "../app/wave-line";

const GITHUB_URL = "https://github.com/superb-audio/superb-sdk";
const INSTALL = "cargo add superb";

// The quick start: a session, a track, a buffer, then audio back with no
// device. The assert is what gets copied; the page shows it as the result.
const PROGRAM = `use superb::primitives::buffers::PlanarBuffer;
use superb::primitives::session::TrackKind;
use superb::primitives::{SampleRate, TimePosition};
use superb::{clip, session, track, transport};

let mut session = session::create("demo.superb", SampleRate::HZ_48K, 256)?;
let lead = track::add(&mut session, "lead", TrackKind::Audio)?;

let mut tone = PlanarBuffer::new(2, 48_000);
tone.fill(0.25);
session.audio.insert("tone.wav", tone);
clip::add(&mut session, lead, "tone.wav", TimePosition::ZERO)?;
track::volume(&mut session, lead, -6.0)?;

transport::start(&mut session)?;
let audio = session::steps::render_blocks(&mut session, 8)?;
assert!(audio.peak() > 0.1);`;

function CopyButton({ value, label }: { value: string; label: string }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1400);
    } catch {
      // clipboard is unavailable outside a secure context
    }
  };

  return (
    <button
      type="button"
      className="copy"
      onClick={copy}
      aria-label={label}
      title={copied ? "Copied" : label}
    >
      {copied ? (
        <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <path
            d="M20 6L9 17l-5-5"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      ) : (
        <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <rect
            x="8"
            y="8"
            width="14"
            height="14"
            rx="2"
            stroke="currentColor"
            strokeWidth="2"
          />
          <path
            d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      )}
    </button>
  );
}

function Str({ children }: { children: string }) {
  return <span className="str">{children}</span>;
}

export function Hero() {
  return (
    <main className="page">
      <h1 className="wordmark">SUPERB</h1>
      <WaveLine className="wave" height={44} />
      <div className="stack">
        <p className="tagline">A DAW engine, in one crate.</p>
          <p className="subtitle">
            Sessions, tracks, clips and a mixer, rendered with no device and
            no interface.
          </p>

          <div className="block">
            <code className="line">
              <span className="prompt">$</span> {INSTALL}
            </code>
            <CopyButton value={INSTALL} label="Copy install command" />
          </div>

          <div className="block result">
            <pre className="pre">
              <code>
                {[
                  "use superb::primitives::buffers::PlanarBuffer;\n",
                  "use superb::primitives::session::TrackKind;\n",
                  "use superb::primitives::{SampleRate, TimePosition};\n",
                  "use superb::{clip, session, track, transport};\n\n",
                  "let mut session = session::create(",
                  <Str key="folder">&quot;demo.superb&quot;</Str>,
                  ", SampleRate::HZ_48K, 256)?;\n",
                  "let lead = track::add(&mut session, ",
                  <Str key="lead">&quot;lead&quot;</Str>,
                  ", TrackKind::Audio)?;\n\n",
                  "let mut tone = PlanarBuffer::new(2, 48_000);\n",
                  "tone.fill(0.25);\n",
                  "session.audio.insert(",
                  <Str key="tone">&quot;tone.wav&quot;</Str>,
                  ", tone);\n",
                  "clip::add(&mut session, lead, ",
                  <Str key="clip">&quot;tone.wav&quot;</Str>,
                  ", TimePosition::ZERO)?;\n",
                  "track::volume(&mut session, lead, -6.0)?;\n\n",
                  "transport::start(&mut session)?;\n",
                  "let audio = session::steps::render_blocks(&mut session, 8)?;\n\n",
                  <span key="mark" className="answerMark">&gt;</span>,
                  " ",
                  <span key="answer" className="answer">peak &gt; 0.1</span>,
                  "\n  ",
                  <span key="meta" className="meta">8 blocks, no sound card</span>,
                ]}
              </code>
            </pre>
            <CopyButton value={PROGRAM} label="Copy example" />
          </div>

          <div className="links">
            <a className="docs" href={GITHUB_URL}>
              Documentation <span className="arrow">↗</span>
            </a>
            <a
              className="iconLink"
              href={GITHUB_URL}
              title="GitHub"
              aria-label="GitHub"
            >
              <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                <path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12" />
              </svg>
            </a>
          </div>
      </div>

      <p className="copyright">© 2026 *^* MOSTFUN</p>
    </main>
  );
}
