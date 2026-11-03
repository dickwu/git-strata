import { expect, test } from 'bun:test';
import type { Upstream } from './types';

// Matches strata-types' serialized Upstream/UpstreamAuto reply, including the
// object target and the freshness timestamp on the containing upstream view.
const fixture = {
  upstream: {
    as_of: '2026-11-02T12:00:00Z',
    clean: false,
    repos: {
      tool: {
        following: true,
        behind: 1,
        capped: false,
        unchecked: false,
        diverged: false,
        base_head: '1111111111111111111111111111111111111111',
        auto: {
          source: 'main',
          state: 'pending',
          since: '2026-11-02T11:59:00Z',
          attempts: 0,
          target: {
            head: '2222222222222222222222222222222222222222',
            tree: '3333333333333333333333333333333333333333',
          },
        },
      },
    },
  },
} satisfies { upstream: Upstream };

test('serialized upstream reply keeps the object target and top-level freshness', () => {
  const reply: { upstream: Upstream } = JSON.parse(JSON.stringify(fixture));
  const repo = reply.upstream.repos.tool;
  expect(repo.auto?.target?.head).toBe(fixture.upstream.repos.tool.auto.target.head);
  expect(repo.auto?.target?.tree).toBe(fixture.upstream.repos.tool.auto.target.tree);
  expect(reply.upstream.as_of).toBe(fixture.upstream.as_of);
  // @ts-expect-error The wire timestamp belongs only to Upstream, never UpstreamRepo.
  expect(repo.as_of).toBeUndefined();
});
