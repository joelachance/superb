import Link from 'next/link';

export default function HomePage() {
  return (
    <div className="home">
      <h1 className="wordmark">SUPERB</h1>
      <p>
        An open source toolkit written in Rust for building audio applications.
        <br />
        BYO user interface, and code a DAW in less than 2000 lines of code.
      </p>
      <div className="home-links">
        <Link href="/docs">Read the docs</Link>
      </div>
    </div>
  );
}
