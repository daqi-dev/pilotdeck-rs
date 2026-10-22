// Compare siblings including implicit directories before touching the host FS.
export function assertPortablePaths(paths) {
  const children = new Map();
  for (const path of paths) {
    let parent = '';
    for (const name of path.split('/')) {
      const key = parent + '/' + name.normalize('NFD').toLowerCase();
      const previous = children.get(key);
      if (previous !== undefined && previous !== name) {
        throw new Error(`non-portable fixture paths: ${parent}/${previous} and ${parent}/${name}`);
      }
      children.set(key, name);
      parent += '/' + name;
    }
  }
}
