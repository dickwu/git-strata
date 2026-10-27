'use client';

import { useEffect, useState, type ReactNode } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { App, ConfigProvider, theme as antdTheme } from 'antd';
import type { MappingAlgorithm } from 'antd';
import { initTheme, useTheme } from '@/stores/theme';
import { initLayout } from '@/stores/layout';
import { TOKENS } from '@/lib/tokens';

export function Providers({ children }: { children: ReactNode }) {
  const [client] = useState(() => new QueryClient());
  const theme = useTheme((s) => s.theme);

  useEffect(() => initTheme(), []);
  useEffect(() => initLayout(), []);
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  const t = TOKENS[theme];
  // antd's algorithms shift the seed colour (dark turns #6FB3D2 into #619bb6). Pin the
  // primary to the §8.2 accent afterwards so the contrast test covers what is drawn.
  const pin: MappingAlgorithm = (_seed, map) => ({
    ...map!,
    colorPrimary: t.accent,
    colorTextLightSolid: t.onAccent,
    // Link buttons (Clear, back) read in the accent text colour, not antd's blue.
    colorLink: t.accentText,
    colorLinkHover: t.accent,
    colorLinkActive: t.accent,
  });
  return (
    <QueryClientProvider client={client}>
      <ConfigProvider
        theme={{
          algorithm: [theme === 'dark' ? antdTheme.darkAlgorithm : antdTheme.defaultAlgorithm, pin],
          token: {
            colorPrimary: t.accent,
            colorBgLayout: t.bgApp,
            colorBgContainer: t.bgPanel,
            colorText: t.text,
            colorTextSecondary: t.textMuted,
            colorError: '#d4493a',
            colorSuccess: '#1f9d55',
            fontFamily: 'var(--font-ui)',
            fontFamilyCode: 'var(--font-mono)',
            fontSize: 13,
            borderRadius: 6,
          },
        }}
      >
        <App>{children}</App>
      </ConfigProvider>
    </QueryClientProvider>
  );
}
