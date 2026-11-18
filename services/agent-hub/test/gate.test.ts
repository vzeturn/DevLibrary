import { describe, expect, it } from 'vitest';
import { evaluateGate, type GateAgent, type GateInput } from '../src/domain/gate';

const H = 3600;
const agent = (over: Partial<GateAgent> = {}): GateAgent => ({
  agent_id: 'a1',
  first_seen: 0,
  last_seen: 3 * H,
  rolled_back: false,
  on_version: true,
  modules: [{ name: 'pda-sync', state: 'running' }],
  ...over,
});
const input = (over: Partial<GateInput> = {}): GateInput => ({
  minAgents: 1,
  soakHours: 2,
  agents: [agent()],
  errorFingerprints: [],
  previousVersion: '1.0.0',
  previousFingerprints: [],
  ...over,
});
const failing = (i: GateInput): string[] => evaluateGate(i).checks.filter((c) => !c.ok).map((c) => c.name);

describe('evaluateGate', () => {
  it('passes when every check holds', () => {
    const r = evaluateGate(input());
    expect(r.pass).toBe(true);
    expect(r.checks.map((c) => c.name)).toEqual(['min_agents', 'soak', 'modules_running', 'no_new_errors', 'no_rollback']);
  });

  it('fails min_agents when too few canary agents run the version', () => {
    expect(failing(input({ minAgents: 2 }))).toEqual(['min_agents', 'soak']);
    expect(failing(input({ agents: [] }))).toEqual(['min_agents', 'soak']);
  });

  it('fails soak when agents have not run the version long enough', () => {
    expect(failing(input({ agents: [agent({ last_seen: 2 * H - 1 })] }))).toEqual(['soak']);
    expect(failing(input({ agents: [agent({ last_seen: 2 * H })] }))).toEqual([]);
  });

  it('fails modules_running when a module on the version is not running', () => {
    const degraded = agent({ modules: [{ name: 'pda-sync', state: 'degraded' }] });
    const r = evaluateGate(input({ agents: [degraded] }));
    expect(r.checks.filter((c) => !c.ok).map((c) => c.name)).toEqual(['modules_running']);
    expect(r.checks.find((c) => c.name === 'modules_running')?.detail).toContain('a1/pda-sync=degraded');
  });

  it('accepts disabled modules and ignores agents that moved off the version', () => {
    expect(failing(input({ agents: [agent({ modules: [{ name: 'x', state: 'disabled' }] })] }))).toEqual([]);
    const moved = agent({ agent_id: 'a2', on_version: false, modules: [{ name: 'x', state: 'stopped' }] });
    expect(failing(input({ agents: [agent(), moved] }))).toEqual([]);
  });

  it('fails no_new_errors only for fingerprints unseen on the previous version', () => {
    expect(failing(input({ errorFingerprints: ['old'], previousFingerprints: ['old'] }))).toEqual([]);
    const r = evaluateGate(input({ errorFingerprints: ['old', 'new1', 'new1'], previousFingerprints: ['old'] }));
    expect(r.pass).toBe(false);
    expect(r.checks.find((c) => c.name === 'no_new_errors')?.detail).toBe('new vs 1.0.0: new1');
  });

  it('treats every error as new when there is no previous version', () => {
    expect(failing(input({ errorFingerprints: ['e'], previousVersion: null }))).toEqual(['no_new_errors']);
  });

  it('fails no_rollback when any agent rolled the version back', () => {
    const rolled = agent({ agent_id: 'a2', rolled_back: true, on_version: false });
    expect(failing(input({ agents: [agent(), rolled] }))).toEqual(['no_rollback']);
  });

  it('does not count rolled-back agents towards min_agents', () => {
    expect(failing(input({ agents: [agent({ rolled_back: true, on_version: false })] }))).toEqual([
      'min_agents',
      'soak',
      'no_rollback',
    ]);
  });
});
