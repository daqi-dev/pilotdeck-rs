// Try every owned cleanup even when an earlier step failed. Steps carry their
// own deadlines; aggregate errors only after every resource has been visited.
export async function cleanupAll(steps) {
  const failures = [];
  for (const step of steps) {
    try { await step(); }
    catch (error) { failures.push(error); }
  }
  if (failures.length) throw new AggregateError(failures, '#1145 owned cleanup failed');
}
