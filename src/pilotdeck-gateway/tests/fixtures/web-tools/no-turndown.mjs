// Makes `turndown` unresolvable for modules loaded with `?no-turndown`, the
// way it fails when the package is absent: Node's own resolver, from a parent
// outside any node_modules tree.
const PARENT = 'file:///pilotdeck/src/tool/builtin/web/urlFetcher.ts';
export async function resolve(specifier, context, nextResolve) {
  if (context.parentURL?.includes('?no-turndown')) {
    if (specifier === 'turndown') return nextResolve(specifier, { ...context, parentURL: PARENT });
    if (specifier.startsWith('.')) {
      const resolved = await nextResolve(specifier, context);
      return { ...resolved, url: `${resolved.url.split('?')[0]}?no-turndown` };
    }
  }
  return nextResolve(specifier, context);
}
