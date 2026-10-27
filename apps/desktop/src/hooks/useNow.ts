'use client';

import { useEffect, useState } from 'react';

/** The current time, re-read every `ms` (client-side clock only; no daemon calls). */
export function useNow(ms = 60_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), ms);
    return () => clearInterval(t);
  }, [ms]);
  return now;
}
