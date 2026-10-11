import type { Metadata } from "next";
import localFont from "next/font/local";
import "./globals.css";

const radwave = localFont({
  src: "./fonts/Radwave.otf",
});

export const metadata: Metadata = {
  title: "Untitled",
};

export default function RootLayout({ children }: LayoutProps<"/">) {
  return (
    <html lang="en" className={radwave.className}>
      <body>{children}</body>
    </html>
  );
}
