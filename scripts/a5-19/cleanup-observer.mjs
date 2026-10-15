/** Attempt all owned cleanup steps and restoration; no failure is swallowed. */
export async function cleanupAll(steps, restore) {
  const errors = [];
  try {
    for (const step of steps) {
      try { await step(); } catch (error) { errors.push(error); }
    }
  } finally {
    try { restore(); } catch (error) { errors.push(error); }
  }
  if (errors.length) throw new AggregateError(errors, 'owned fixture cleanup failed');
}
