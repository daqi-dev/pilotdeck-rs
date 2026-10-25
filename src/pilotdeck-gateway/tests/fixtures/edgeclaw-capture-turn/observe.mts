// Scenario definitions shared by generate.mts and verify.mts. Both run the
// pinned TS DefaultContextRuntime and MemoryResolver conversion; nothing here
// synthesizes expected bytes.
export const text = (value: string) => ({ type: 'text', text: value });
export const user = (...content: any[]) => ({ role: 'user', content });
export const assistant = (...content: any[]) => ({ role: 'assistant', content });
export const toolResult = (toolCallId: string, content: any[]) => ({
  type: 'tool_result',
  toolCallId,
  content,
});

const fork = (forkCarryover: any) => ({ metadata: { forkCarryover } });

export const scenarios = () => [
  {
    name: 'no-resolver',
    resolver: 'none',
    sessionId: 'session',
    messages: [user(text('hello'))],
    errored: false,
  },
  {
    name: 'normal-turn',
    resolver: 'recording',
    projectRoot: '/pdrs-fixture/project',
    sessionId: 'session',
    messages: [
      user(text('u1')),
      assistant(text('a1')),
      { role: 'tool', content: [toolResult('call-1', [text(' out ')])] },
    ],
    errored: false,
  },
  {
    name: 'errored-turn',
    resolver: 'recording',
    projectRoot: '/pdrs-fixture/project',
    sessionId: 'session',
    messages: [user(text('failed'))],
    errored: true,
  },
  {
    name: 'no-project-root',
    resolver: 'recording',
    sessionId: 'session',
    messages: [user(text('no root'))],
    errored: false,
  },
  ...['discovery', 'workspace', 'execute', 'report', 'apply'].map((kind) => ({
    name: `always-on-${kind}`,
    resolver: 'recording',
    projectRoot: '/pdrs-fixture/project',
    sessionId: `always-on/${kind}:task-1`,
    messages: [user(text('skipped'))],
    errored: false,
  })),
  {
    name: 'always-on-prefix-without-colon',
    resolver: 'recording',
    projectRoot: '/pdrs-fixture/project',
    sessionId: 'always-on/discovery',
    messages: [user(text('captured'))],
    errored: false,
  },
  {
    name: 'unknown-session-prefix',
    resolver: 'recording',
    projectRoot: '/pdrs-fixture/project',
    sessionId: 'always-on/unknown:task-1',
    messages: [user(text('captured'))],
    errored: false,
  },
  {
    name: 'fork-carryover-truthiness',
    resolver: 'recording',
    projectRoot: '/pdrs-fixture/project',
    sessionId: 'session',
    messages: [
      { ...user(text('object')), ...fork({ sourceSessionId: 'old' }) },
      { ...user(text('false')), ...fork(false) },
      { ...user(text('zero')), ...fork(0) },
      { ...user(text('empty-string')), ...fork('') },
      { ...user(text('empty-array')), ...fork([]) },
      { ...user(text('null')), ...fork(null) },
      { ...user(text('string')), ...fork('yes') },
    ],
    errored: false,
  },
  {
    name: 'throwing-resolver',
    resolver: 'throwing',
    projectRoot: '/pdrs-fixture/project',
    sessionId: 'session',
    messages: [user(text('boom'))],
    errored: false,
  },
  {
    name: 'rejecting-resolver',
    resolver: 'rejecting',
    projectRoot: '/pdrs-fixture/project',
    sessionId: 'session',
    messages: [user(text('reject'))],
    errored: true,
  },
];

export const conversions = () => [
  {
    name: 'text-merge-and-msgids',
    includeForkCarryover: undefined,
    messages: [
      user(text(' a '), text('b '), text('   ')),
      assistant(text('x'), text('y')),
      user(text('z')),
      assistant(text('')),
    ],
  },
  {
    name: 'tool-blocks',
    includeForkCarryover: undefined,
    messages: [
      user(
        toolResult('call-1', [
          text(' out '),
          { type: 'image', source: 'base64', data: 'AA==', mimeType: 'image/png' },
          text('tail'),
        ]),
      ),
      assistant(
        { type: 'tool_result_reference', toolCallId: 'call-1', preview: ' preview ' },
        { type: 'media_reference', toolCallId: 'call-1', mediaType: 'image', preview: ' media ' },
      ),
      user({ type: 'image', source: 'base64', data: 'AA==', mimeType: 'image/png' }, text('after')),
      assistant({ type: 'tool_call', id: 'call-2', name: 'lookup', input: {} }),
      user(toolResult('call-2', [text(''), { type: 'pdf' }])),
    ],
  },
  {
    name: 'fork-include-default',
    includeForkCarryover: undefined,
    messages: [
      { ...user(text('kept')), ...fork({ sourceSessionId: 'old' }) },
      { ...user(text('also-kept')), ...fork(false) },
    ],
  },
  {
    name: 'fork-exclude',
    includeForkCarryover: false,
    messages: [
      { ...user(text('dropped-object')), ...fork({ sourceSessionId: 'old' }) },
      { ...user(text('kept-false')), ...fork(false) },
      { ...user(text('dropped-zero-string')), ...fork('0') },
      { ...user(text('kept-empty-string')), ...fork('') },
      { ...user(text('dropped-empty-array')), ...fork([]) },
      { ...user(text('kept-null')), ...fork(null) },
      { ...user(text('kept-undefined')), metadata: {} },
    ],
  },
  {
    name: 'unicode-whitespace-and-astral',
    includeForkCarryover: undefined,
    messages: [
      user(text('\u00a0\ufeff hello 😀 \u3000')),
      user(text('\u0009\u000a')),
      assistant(text('\u2003wide\u2003'), text('🚀')),
    ],
  },
];

export async function runScenario(
  DefaultContextRuntime: any,
  convert: any,
  spec: any,
): Promise<any> {
  const calls: any[] = [];
  let invoked = false;
  let throwing: any;
  let rejecting: any;
  let resolver: any;
  if (spec.resolver === 'recording') {
    resolver = {
      async retrieve() {
        return { diagnostics: [] };
      },
      async captureTurn(input: any) {
        invoked = true;
        calls.push({
          sessionId: input.sessionId,
          projectRoot: input.projectRoot,
          errored: input.errored,
          messages: convert(input.messages, { includeForkCarryover: false }),
        });
      },
    };
  } else if (spec.resolver === 'throwing') {
    resolver = {
      async retrieve() {
        return { diagnostics: [] };
      },
      captureTurn() {
        invoked = true;
        throwing = new Error('capture boom');
        throw throwing;
      },
    };
  } else if (spec.resolver === 'rejecting') {
    resolver = {
      async retrieve() {
        return { diagnostics: [] };
      },
      captureTurn() {
        invoked = true;
        rejecting = new Error('capture reject');
        return Promise.reject(rejecting);
      },
    };
  }
  const options: any = {};
  if (spec.projectRoot !== undefined) options.projectRoot = spec.projectRoot;
  if (resolver) options.memoryResolver = resolver;
  const runtime = new DefaultContextRuntime(options);
  let runnerThrew: any = null;
  try {
    await runtime.captureTurn({
      sessionId: spec.sessionId,
      turnId: 'turn',
      messages: spec.messages,
      errored: spec.errored,
    });
  } catch (error: any) {
    runnerThrew = error instanceof Error ? error.message : String(error);
  }
  return {
    name: spec.name,
    supportsCaptureTurn: typeof runtime.captureTurn === 'function',
    invoked,
    calls,
    throws: throwing ? throwing.message : null,
    rejects: rejecting ? rejecting.message : null,
    runnerThrew,
  };
}

export function runConversion(convert: any, spec: any): any {
  const options =
    spec.includeForkCarryover === undefined
      ? {}
      : { includeForkCarryover: spec.includeForkCarryover };
  return convert(spec.messages, options);
}

/// Real TS manifest synthesis for a composition whose context execution is the
/// real DefaultContextRuntime. The advertised method table therefore reflects
/// `typeof runtime.captureTurn` exactly as the dispatcher sees it.
export async function runManifest(
  {
    DefaultContextRuntime,
    createSidecarDefaultModuleDispatcher,
    HostToolCheckpoint,
  }: any,
  convert: any,
): Promise<any> {
  const resolver = {
    async retrieve() {
      return { diagnostics: [] };
    },
    async captureTurn() {},
  };
  const runtime = new DefaultContextRuntime({
    projectRoot: '/pdrs-fixture/manifest',
    memoryResolver: resolver,
  });
  const dispatcher = createSidecarDefaultModuleDispatcher({
    config: {
      cwd: '/pdrs-fixture/project',
      permissionMode: 'default',
      permissionContext: {},
      env: {},
    },
    input: { sessionId: 'session', turnId: 'turn', messages: [] },
    checkpoint: new HostToolCheckpoint({}),
    capabilityResultObserver: { async onCapabilityResults() {} },
    planTodoHandler: async () => ({}),
    modules: {
      model: { execution: {} },
      capability: {
        execution: { list: () => [] },
        runtimeContext: {
          bindTurn: () => ({
            permissionContext: () => ({}),
            toolRuntimeContext: () => ({}),
            executionContext: () => ({}),
            contextIdentity: (source: any) => ({ ...(source ?? {}) }),
          }),
        },
      },
      context: {
        execution: runtime,
        requestIdentity: {
          bindTurn: () => ({ contextIdentity: (source: any) => ({ ...(source ?? {}) }) }),
        },
      },
    },
  });
  return {
    captureTurnType: typeof runtime.captureTurn,
    hostModulesContext: dispatcher.manifest.hostModules.context,
  };
}
