'use client';

import { SearchOutlined } from '@ant-design/icons';
import { copy } from '@/lib/copy';
import { useNav } from '@/stores/nav';

/** 36 px; the whole bar drags the window (needs core:window:allow-start-dragging). */
export function Titlebar() {
  const setPalette = useNav((s) => s.setPalette);
  return (
    <header className="titlebar" data-tauri-drag-region>
      <span data-tauri-drag-region style={{ fontFamily: 'var(--font-title)', fontWeight: 600 }}>
        {copy.app}
      </span>
      <span data-tauri-drag-region style={{ flex: 1, alignSelf: 'stretch' }} />
      <button
        type="button"
        className="nav-item"
        style={{ width: 'auto', height: 24, gap: 6 }}
        onClick={() => setPalette(true)}
        aria-label={copy.palette.open}
      >
        <SearchOutlined />
        <span className="muted">⌘K</span>
      </button>
    </header>
  );
}
