'use client';

import { useRef, type KeyboardEvent } from 'react';
import { Tooltip } from 'antd';
import { DesktopOutlined, MoonOutlined, SunOutlined } from '@ant-design/icons';
import { copy } from '@/lib/copy';
import { THEME_CHOICES, useTheme, type ThemeChoice } from '@/stores/theme';
import css from './ThemeSwitch.module.css';

const ICON: Record<ThemeChoice, typeof SunOutlined> = {
  system: DesktopOutlined,
  light: SunOutlined,
  dark: MoonOutlined,
};

/**
 * Status bar theme switch (t93u): System / Light / Dark as a three-stop segmented track.
 * It writes the theme store, the one source of truth that ⌘K and Providers also use.
 */
export function ThemeSwitch() {
  const choice = useTheme((s) => s.choice);
  const system = useTheme((s) => s.system);
  const setChoice = useTheme((s) => s.setChoice);
  const refs = useRef<Array<HTMLButtonElement | null>>([]);
  const index = THEME_CHOICES.indexOf(choice);

  // Radio group keys: arrows move and select, wrapping; Home / End jump.
  const onKey = (e: KeyboardEvent) => {
    const n = THEME_CHOICES.length;
    const next =
      e.key === 'ArrowRight' || e.key === 'ArrowDown'
        ? (index + 1) % n
        : e.key === 'ArrowLeft' || e.key === 'ArrowUp'
          ? (index - 1 + n) % n
          : e.key === 'Home'
            ? 0
            : e.key === 'End'
              ? n - 1
              : -1;
    if (next < 0) return;
    e.preventDefault();
    setChoice(THEME_CHOICES[next]);
    refs.current[next]?.focus();
  };

  return (
    <div
      role="radiogroup"
      aria-label={copy.theme.label}
      className={css.track}
      style={{ ['--i' as string]: index }}
      onKeyDown={onKey}
      data-testid="theme-switch"
    >
      <span className={css.thumb} aria-hidden />
      {THEME_CHOICES.map((c, i) => {
        const Icon = ICON[c];
        const tip =
          c === 'system'
            ? `${copy.theme.tip.system} · ${copy.theme.now(copy.theme.names[system])}`
            : copy.theme.tip[c];
        return (
          <Tooltip key={c} title={tip} placement="topRight" mouseEnterDelay={0.35}>
            <button
              ref={(el) => {
                refs.current[i] = el;
              }}
              type="button"
              role="radio"
              aria-checked={c === choice}
              aria-label={copy.theme.names[c]}
              tabIndex={c === choice ? 0 : -1}
              className={css.seg}
              onClick={() => setChoice(c)}
              data-testid={`theme-${c}`}
            >
              <Icon />
            </button>
          </Tooltip>
        );
      })}
    </div>
  );
}
