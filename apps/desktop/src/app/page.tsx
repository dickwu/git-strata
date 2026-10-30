'use client';

import { Providers } from './providers';
import { Shell } from '@/components/Shell';

export default function Page() {
  return (
    <Providers>
      <Shell />
    </Providers>
  );
}
