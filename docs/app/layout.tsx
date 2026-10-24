import { RootProvider } from 'fumadocs-ui/provider/next';
import './global.css';
import localFont from 'next/font/local';

const radwave = localFont({
  src: './fonts/Radwave.otf',
  variable: '--font-radwave',
  display: 'block',
});

const berkeleyMono = localFont({
  src: './fonts/BerkeleyMono.otf',
  variable: '--font-berkeley',
  display: 'swap',
});

export default function Layout({ children }: LayoutProps<'/'>) {
  return (
    <html
      lang="en"
      className={`${radwave.variable} ${berkeleyMono.variable}`}
      suppressHydrationWarning
    >
      <body className="flex flex-col min-h-screen">
        <RootProvider theme={{ forcedTheme: 'light', enableSystem: false }}>
          {children}
        </RootProvider>
      </body>
    </html>
  );
}
