import type { Metadata } from "next";
import localFont from "next/font/local";
import "./globals.css";

const radwave = localFont({
  src: "./fonts/Radwave.otf",
  variable: "--font-radwave",
  display: "block",
});

const berkeleyMono = localFont({
  src: "./fonts/BerkeleyMono.otf",
  variable: "--font-berkeley",
  display: "swap",
});

export const metadata: Metadata = {
  title: "Superb — a DAW engine, in one crate",
  description:
    "Sessions, tracks, clips and a mixer, rendered with no device and no interface.",
};

export default function RootLayout({ children }: LayoutProps<"/">) {
  return (
    <html
      lang="en"
      className={`${radwave.variable} ${berkeleyMono.variable}`}
    >
      <body>{children}</body>
    </html>
  );
}
