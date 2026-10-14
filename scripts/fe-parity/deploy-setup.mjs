import { requireUI } from './env.mjs';
import { assertDeployExecutionEnv, deployCell, deployOrigin } from './deploy-matrix.mjs';
export default async function setup() {
  assertDeployExecutionEnv(process.env);
  const cell = deployCell(process.env.FE_B_CELL);
  if (cell.kind !== 'auth') return;
  const context = await requireUI('@playwright/test').request.newContext({ baseURL: deployOrigin });
  try {
    // Playwright APIResponse uses ok()/status(); Node fetch Response does not.
    const reply = await context.post('/api/auth/register', { data: { username: `evidence-${cell.profile}`, password: 'browser-evidence-password' } });
    if (!reply.ok()) throw new Error(`B fresh account setup failed: ${reply.status()}`);
    const { token } = await reply.json();
    if (typeof token !== 'string' || !token) throw new Error('B native registration did not return token');
    const done = await context.post('/api/user/complete-onboarding', { headers: { authorization: `Bearer ${token}` } });
    if (!done.ok()) throw new Error(`B complete onboarding failed: ${done.status()}`);
  } finally { await context.dispose(); }
}
