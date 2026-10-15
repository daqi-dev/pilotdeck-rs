/** Passive append tap: original promise identity; observer faults fail drain. */
export function appendObserver(original, { commit, failure }) {
  const pending = new Set(), observers = new Set(), errors = [];
  const observe = (callback, value) => {
    try { callback(value); } catch (error) { errors.push(error); }
  };
  const append = input => {
    let write;
    try { write = original(input); }
    catch (error) { observe(e => failure(input, e), error); throw error; }
    pending.add(write);
    const observer = write.then(
      () => observe(() => commit(input)),
      error => observe(e => failure(input, e), error),
    ).then(() => { pending.delete(write); observers.delete(observer); });
    observers.add(observer);
    return write;
  };
  const drain = async () => {
    while (pending.size || observers.size) await Promise.allSettled([...pending, ...observers]);
    if (errors.length) throw new AggregateError(errors, 'append observer failed');
  };
  return { append, drain, pending, errors };
}
