import type { BaseLayoutProps } from 'fumadocs-ui/layouts/shared';
import { gitConfig } from './shared';

export function baseOptions(): BaseLayoutProps {
  return {
    nav: {
      title: <span className="wordmark">SUPERB</span>,
    },
    themeSwitch: {
      enabled: false,
    },
    githubUrl: gitConfig.user ? `https://github.com/${gitConfig.user}/${gitConfig.repo}` : undefined,
  };
}
