/**
 * Which AI agent made the session. Nothing renders while the daemon does not record agents
 * (every session today), so the list does not fill with "unknown".
 */
export function AgentBadge({ agent }: { agent: string | null }) {
  if (!agent) return null;
  return (
    <span className="agent-badge" data-agent={agentKind(agent)} title={`Agent: ${agent}`}>
      {agent}
    </span>
  );
}

/** Known families get a stable hue; anything else is neutral. */
export function agentKind(agent: string): 'claude' | 'codex' | 'cursor' | 'other' {
  const a = agent.toLowerCase();
  if (a.includes('claude')) return 'claude';
  if (a.includes('codex')) return 'codex';
  if (a.includes('cursor')) return 'cursor';
  return 'other';
}
