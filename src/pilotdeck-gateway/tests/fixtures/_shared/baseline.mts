// Compatibility entry point; all provenance enforcement lives in generator_guard.
import { bindBaseline, recordedSourceHashes } from './generator_guard.mjs';
import { sourceClosureSha256 } from '../../baseline_source_guard.mjs';

export const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';

export function assertBaseline(baseline: string): string {
  return bindBaseline({ baseline, commit: BASELINE_COMMIT }).sourceCommit;
}

export async function baselineSourceHashes(baseline: string, paths: string[]): Promise<Record<string, string>> {
  const { git, gitExecutable } = bindBaseline({ baseline, commit: BASELINE_COMMIT });
  recordedSourceHashes({ baseline, commit: BASELINE_COMMIT, git }, paths);
  return sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, paths);
}
