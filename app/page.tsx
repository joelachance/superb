import localFont from "next/font/local";
import { WaveLine } from "./wave-line";

const berkeleyMono = localFont({
  src: "./fonts/BerkeleyMono.otf",
});

export default function Home() {
  return (
    <main className="flex min-h-dvh flex-col">
      <header className="p-6">
        <p className="m-0 text-2xl leading-none text-black">SUPERB</p>
      </header>
      <WaveLine />
      <footer className="mt-auto p-6">
        <p className="m-0 text-sm leading-none text-black">Superb</p>
        <p
          className={`${berkeleyMono.className} m-0 mt-2 text-xs leading-none text-black`}
        >
          © 2026 Most Fun Corporation
        </p>
      </footer>
    </main>
  );
}
