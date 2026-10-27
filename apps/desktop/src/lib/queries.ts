'use client';

import { useEffect, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { call, isDaemonDown } from './tauri';
import { pollPlan, type PollPlan } from './poll';
import { daemonView } from './view';
import type { DaemonStatus, DepsList, DuReply, StatusReply } from './types';

function useVisible(): boolean {
  const [v, setV] = useState(
    () => typeof document === 'undefined' || document.visibilityState === 'visible'
  );
  useEffect(() => {
    const on = () => setV(document.visibilityState === 'visible');
    document.addEventListener('visibilitychange', on);
    return () => document.removeEventListener('visibilitychange', on);
  }, []);
  return v;
}

// The back end allows one request per kind in flight and times out on its own
// (src-tauri/src/daemon.rs), so the front end never retries on top of it.
const base = { retry: false, refetchOnWindowFocus: true, staleTime: 0 } as const;

export function useDaemonStatus(interval: number | false) {
  return useQuery<DaemonStatus>({
    ...base,
    queryKey: ['daemonStatus'],
    queryFn: () => call<DaemonStatus>('daemon_status'),
    refetchInterval: interval,
  });
}

/** The poll plan for this render, from the latest daemon_status outcome. */
export function usePlan(): { plan: PollPlan; status: ReturnType<typeof useDaemonStatus> } {
  const visible = useVisible();
  const [plan, setPlan] = useState<PollPlan>(() => pollPlan({ visible, up: true, legacy: true }));
  const status = useDaemonStatus(plan.daemonStatus);
  // Only "daemon_down" counts as down; busy or timed-out replies keep the normal plan.
  const up = !(status.isError && isDaemonDown(status.error));
  const legacy = status.data ? daemonView(status.data).legacy : true;
  useEffect(() => {
    setPlan(pollPlan({ visible, up, legacy }));
  }, [visible, up, legacy]);
  return { plan, status };
}

export function useSessions(interval: number | false) {
  return useQuery<StatusReply>({
    ...base,
    queryKey: ['sessions'],
    queryFn: () => call<StatusReply>('sessions_list'),
    refetchInterval: interval,
    enabled: interval !== false,
  });
}

export function useSession(id: string | null, interval: number | false) {
  return useQuery<StatusReply>({
    ...base,
    queryKey: ['session', id],
    queryFn: () => call<StatusReply>('session_status', { session: id }),
    refetchInterval: interval,
    enabled: id !== null && interval !== false,
  });
}

export function useDeps(interval: number | false) {
  return useQuery<DepsList>({
    ...base,
    queryKey: ['deps'],
    queryFn: () => call<DepsList>('deps_list'),
    refetchInterval: interval,
    enabled: interval !== false,
  });
}

export function useDu(interval: number | false) {
  return useQuery<DuReply>({
    ...base,
    queryKey: ['du'],
    queryFn: () => call<DuReply>('du'),
    refetchInterval: interval,
    enabled: interval !== false,
  });
}

export function useSocket() {
  return useQuery<string>({
    ...base,
    queryKey: ['socket'],
    queryFn: () => call<string>('daemon_socket'),
    staleTime: Infinity,
  });
}
