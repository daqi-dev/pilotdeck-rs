// Calls the fixed TS baseline directly. Every expected value in oracle.json
// comes from these calls; no value is synthesized by the Rust side.
import {existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {tmpdir} from 'node:os';
import {dirname, join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {freezeClock} from '../edgeclaw-file-memory/observe.mts';

export const HOME = '/fixture/home';
const ROOT = 'src/context/memory/edgeclaw-memory-core/src';
const ENV_KEYS = [
  'EDGECLAW_MEMORY_MODEL', 'EDGECLAW_MEMORY_PROVIDER', 'EDGECLAW_MEMORY_BASE_URL',
  'EDGECLAW_MEMORY_API_KEY', 'EDGECLAW_MEMORY_API_TYPE',
  'OPENAI_MODEL', 'OPENAI_BASE_URL', 'OPENAI_API_KEY', 'ENV_KEY', 'LOOKS_LIKE_ENV',
];

export function enc(v: any): any {
  if (v === undefined) return {$undefined: true};
  if (typeof v === 'number' && (Object.is(v, -0) || !Number.isFinite(v))) return {$number: Object.is(v, -0) ? '-0' : String(v)};
  if (Array.isArray(v)) return v.map(enc);
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, enc(x)]));
  return v;
}

function clean(v: any, root: string): any {
  if (typeof v === 'string') return v.split(root).join('$SCRATCH');
  if (Array.isArray(v)) return v.map((x) => clean(x, root));
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, clean(x, root)]));
  return v;
}

function dataUrl(code: string): string {
  return 'data:text/javascript;base64,' + Buffer.from(code).toString('base64');
}

function rewrite(code: string, sourcePath: string, baseline: string): string {
  const packageRoot = join(baseline, ROOT, '..');
  return code.replace(/from (["'])([^"']+)\1/g, (_match: string, quote: string, specifier: string) => {
    let file: string;
    if (specifier === 'edgeclaw-memory-core') {
      file = join(packageRoot, 'lib/index.js');
    } else if (specifier.startsWith('.')) {
      file = resolve(dirname(sourcePath), specifier);
      if (file.endsWith('.js') && existsSync(file.slice(0, -3) + '.ts')) file = file.slice(0, -3) + '.ts';
    } else {
      return _match;
    }
    return 'from ' + quote + pathToFileURL(file).href + quote;
  });
}

function transformed(baseline: string, sourcePath: string, append: string, tag: string): string {
  const requireFromBaseline = createRequire(join(baseline, 'package.json'));
  const {transformSync} = createRequire(requireFromBaseline.resolve('tsx'))('esbuild');
  const code = transformSync(
    readFileSync(sourcePath, 'utf8') + '\n' + append + `\nconst __tag = ${JSON.stringify(tag)};\n`,
    {loader: 'ts', format: 'esm', target: 'es2022', sourcefile: sourcePath},
  ).code;
  return rewrite(code, sourcePath, baseline);
}

async function loadService(baseline: string, tag: string) {
  const sourcePath = join(baseline, ROOT, 'service.ts');
  const code = transformed(
    baseline,
    sourcePath,
    'export { buildLlmConfig, resolveOpenClawModelConfig, mergeIndexingSettings };',
    'svc-' + tag,
  );
  return import(dataUrl(code));
}

async function loadExtractor(baseline: string) {
  const sourcePath = join(baseline, ROOT, 'core/skills/llm-extraction.ts');
  const code = transformed(
    baseline,
    sourcePath,
    `export { sanitizeHeaders, parseModelRef as parseExtractorModelRef, resolveAgentPrimaryModel, looksLikeEnvVarName };
export function __selection(config: any, agentId?: string) { return new LlmMemoryExtractor(config, undefined).resolveSelection(agentId); }
export async function __apiKey(config: any, provider: string, runtime: any) { return new LlmMemoryExtractor(config, runtime).resolveApiKey(provider); }`,
    'extractor',
  );
  return import(dataUrl(code));
}

async function loadFactory(baseline: string) {
  const sourcePath = join(baseline, 'src/context/memory/createEdgeClawMemoryProviderFromConfig.ts');
  const code = transformed(
    baseline,
    sourcePath,
    'export { resolveMemoryLlm, memoryApiTypeForProtocol };',
    'factory',
  );
  return import(dataUrl(code));
}

function valid(json: any) {
  return {kind: 'valid', json, text: JSON.stringify(json, null, 2)};
}

function writeOpenClaw(home: string, state: any) {
  const directory = join(home, '.openclaw');
  mkdirSync(directory, {recursive: true});
  const path = join(directory, 'openclaw.json');
  if (state.kind === 'missing') {
    if (existsSync(path)) rmSync(path);
    return;
  }
  writeFileSync(path, state.text);
}

async function withEnv<T>(env: Record<string, string>, body: () => Promise<T>): Promise<T> {
  const saved = Object.fromEntries(ENV_KEYS.map((key) => [key, process.env[key]]));
  try {
    for (const key of ENV_KEYS) {
      if (env[key] === undefined) delete process.env[key];
      else process.env[key] = env[key]!;
    }
    return await body();
  } finally {
    for (const key of ENV_KEYS) {
      if (saved[key] === undefined) delete process.env[key];
      else process.env[key] = saved[key]!;
    }
  }
}

const EMPTY_PROVIDERS = {
  models: {providers: {
    openai: {
      api: 'openai-responses',
      baseUrl: ' https://openai.example/v1/ ',
      apiKey: ' OPENCLAW_API_KEY ',
      headers: {'X-Provider': ' provider ', 'X-Blank': '   ', 'X-Number': 7},
      models: [
        {id: 'gpt-5', name: 'GPT Five', api: 'openai-completions', baseUrl: ' https://model.example/v1// ',
         headers: {'X-Model': ' model ', 'X-Provider': ' model-wins '}},
        {id: 'gpt-5-mini', name: 'GPT Five Mini'},
      ],
    },
    anthropic: {
      baseUrl: 'https://anthropic.example',
      api: 'anthropic',
      apiKey: 'ANTHROPIC_KEY',
      models: [{id: 'claude-sonnet', name: 'Claude Sonnet'}],
    },
  }},
  agents: {defaults: {model: {primary: 'openai/gpt-5'}}},
};

export async function collect(baseline: string) {
  freezeClock();
  Math.random = () => 0.5;
  const scratch = mkdtempSync(join(tmpdir(), 'pdrs-edgeclaw-llm-config-'));
  const cases: any[] = [];
  const add = (fn: string, args: any[], expected: any) =>
    cases.push({fn, args: enc(args), expected: enc(clean(expected, scratch))});
  const savedHome = process.env.HOME;
  let index = 0;
  try {
    // -- openclaw cache + buildLlmConfig -----------------------------------
    const cacheStates = [
      {name: 'bad-then-valid', states: [{kind: 'bad', text: '{"models":'}, valid(EMPTY_PROVIDERS)]},
      {name: 'missing-then-valid', states: [{kind: 'missing'}, valid(EMPTY_PROVIDERS)]},
      {name: 'valid-then-other', states: [valid(EMPTY_PROVIDERS), valid({models: {providers: {other: {models: [{id: 'x'}]}}}})]},
    ];
    for (const scenario of cacheStates) {
      const home = join(scratch, `cache-${index++}`);
      mkdirSync(home, {recursive: true});
      writeOpenClaw(home, scenario.states[0]!);
      process.env.HOME = home;
      const service = await loadService(baseline, scenario.name);
      const observed = [];
      for (const state of scenario.states) {
        writeOpenClaw(home, state);
        observed.push(service.resolveOpenClawModelConfig());
      }
      add('resolveOpenClawModelConfig', [scenario.states], observed);
    }

    const openClawJson = [
      {name: 'primary-hit', json: EMPTY_PROVIDERS},
      {name: 'no-primary', json: {
        models: {providers: {alpha: {api: 'anthropic', models: [{id: 'm1', name: 'M One'}]}}},
      }},
      {name: 'model-by-name', json: {
        models: {providers: {alpha: {models: [{id: 'id1', name: 'friendly'}, {id: 'id2'}]}}},
        agents: {defaults: {model: {primary: 'alpha/friendly'}}},
      }},
      {name: 'primary-provider-miss', json: {
        models: {providers: {alpha: {api: 'google', models: [{id: 'm1'}]}}},
        agents: {defaults: {model: {primary: 'missing/m1'}}},
      }},
      {name: 'primary-model-miss', json: {
        models: {providers: {alpha: {models: [{id: 'm1'}, {id: 'm2'}]}}},
        agents: {defaults: {model: {primary: 'alpha/nope'}}},
      }},
      {name: 'provider-model-empty', json: {
        models: {providers: {alpha: {baseUrl: 'https://p', models: []}}},
        agents: {defaults: {model: {primary: 'alpha/kept'}}},
      }},
      {name: 'providers-empty', json: {models: {providers: {}}}},
      {name: 'providers-not-record', json: {models: {providers: []}}},
      {name: 'models-missing', json: {agents: {defaults: {model: {primary: 'alpha/x'}}}}},
      {name: 'api-from-provider', json: {
        models: {providers: {alpha: {api: 'anthropic', baseUrl: 'https://a', apiKey: 'k', models: [{id: 'm'}]}}},
      }},
      {name: 'primary-second-provider', json: {
        models: {providers: {
          alpha: {models: [{id: 'a1'}]},
          beta: {api: 'google', models: [{id: 'b2', name: 'B Two'}]},
        }},
        agents: {defaults: {model: {primary: 'beta/b2'}}},
      }},
      // Name matching must select the second model; the first model's
      // api/baseUrl/headers make the `id`-only fallback observable.
      {name: 'model-by-name-second', json: {
        models: {providers: {alpha: {models: [
          {id: 'other', api: 'google', baseUrl: 'https://first.example', headers: {'X-Pick': ' first '}},
          {id: 'id2', name: 'friendly', api: 'anthropic', baseUrl: ' https://second.example// ',
           headers: {'X-Pick': ' second ', 'X-Only': 'second'}},
        ]}}},
        agents: {defaults: {model: {primary: 'alpha/friendly'}}},
      }},
    ];
    const buildCases: Array<{name: string; openClaw: any; options: any; env: Record<string, string>}> = [
      {name: 'defaults', openClaw: {kind: 'missing'}, options: undefined, env: {}},
      {name: 'env-chain', openClaw: {kind: 'missing'}, options: undefined, env: {
        EDGECLAW_MEMORY_MODEL: ' env-provider / env-model ', EDGECLAW_MEMORY_BASE_URL: ' https://env.example/v1/ ',
        EDGECLAW_MEMORY_API_KEY: ' env-key ', EDGECLAW_MEMORY_API_TYPE: ' anthropic ',
        OPENAI_MODEL: 'openai-model', OPENAI_BASE_URL: 'https://openai.example', OPENAI_API_KEY: 'openai-key',
      }},
      {name: 'options-win', openClaw: {kind: 'missing'}, options: {
        provider: ' opt-provider ', model: ' opt-model ', modelRef: 'ref-provider/ref-model',
        baseUrl: ' https://opt.example// ', apiKey: ' opt-key ', apiType: ' google ',
        headers: {'X-Opt': ' opt ', 'X-Dup': ' opt-dup '},
      }, env: {
        EDGECLAW_MEMORY_PROVIDER: 'env-provider', EDGECLAW_MEMORY_MODEL: 'env-model',
        EDGECLAW_MEMORY_BASE_URL: 'https://env', EDGECLAW_MEMORY_API_KEY: 'env-key',
        EDGECLAW_MEMORY_API_TYPE: 'anthropic',
      }},
      {name: 'modelref-only', openClaw: {kind: 'missing'}, options: {modelRef: ' /with/slash '}, env: {}},
      {name: 'openclaw-hit', openClaw: valid(EMPTY_PROVIDERS), options: undefined, env: {}},
      {name: 'openclaw-provider-override', openClaw: valid(EMPTY_PROVIDERS), options: {provider: 'override'}, env: {}},
      {name: 'openclaw-same-selection', openClaw: valid(EMPTY_PROVIDERS), options: {
        provider: 'openai', model: 'gpt-5', headers: {'X-Opt': ' opt ', 'X-Provider': ' opt-wins '},
      }, env: {OPENAI_API_KEY: 'openai-fallback'}},
      {name: 'openclaw-env-base', openClaw: valid(EMPTY_PROVIDERS), options: undefined, env: {
        EDGECLAW_MEMORY_BASE_URL: 'https://env-base', OPENAI_API_KEY: 'openai-key',
      }},
      {name: 'api-key-env-literal', openClaw: {kind: 'missing'}, options: {apiKey: 'LOOKS_LIKE_ENV'}, env: {LOOKS_LIKE_ENV: 'not-used-by-build'}},
      {name: 'empty-env-values', openClaw: {kind: 'missing'}, options: undefined, env: {
        EDGECLAW_MEMORY_PROVIDER: '', EDGECLAW_MEMORY_MODEL: '   ', OPENAI_MODEL: ' final-model ',
      }},
      {name: 'primary-miss-fallback', openClaw: valid(openClawJson[4]!.json), options: undefined, env: {}},
      {name: 'openai-only', openClaw: {kind: 'missing'}, options: undefined, env: {
        OPENAI_MODEL: ' openai-only-model ', OPENAI_BASE_URL: ' https://openai-only ', OPENAI_API_KEY: ' openai-only-key ',
      }},
      {name: 'env-provider-only', openClaw: {kind: 'missing'}, options: undefined, env: {
        EDGECLAW_MEMORY_PROVIDER: ' env-only-provider ',
      }},
      {name: 'openclaw-override-openai', openClaw: valid(EMPTY_PROVIDERS), options: {provider: 'override'}, env: {
        OPENAI_BASE_URL: 'https://override-base', OPENAI_API_KEY: 'override-key',
      }},
      {name: 'empty-option-api-key', openClaw: {kind: 'missing'}, options: {apiKey: '   '}, env: {
        EDGECLAW_MEMORY_API_KEY: ' env-after-empty ', EDGECLAW_MEMORY_MODEL: 'p/m',
      }},
    ];

    for (const scenario of buildCases) {
      const home = join(scratch, `build-${index++}`);
      mkdirSync(home, {recursive: true});
      writeOpenClaw(home, scenario.openClaw);
      process.env.HOME = home;
      const observed = await withEnv(scenario.env, async () => {
        const service = await loadService(baseline, scenario.name);
        return service.buildLlmConfig(scenario.options);
      });
      add('buildLlmConfig', [scenario.options, scenario.env, scenario.openClaw], observed);
    }

    // Direct openclaw observations for the same JSON shapes.
    for (const scenario of openClawJson) {
      const home = join(scratch, `openclaw-${index++}`);
      mkdirSync(home, {recursive: true});
      const state = valid(scenario.json);
      writeOpenClaw(home, state);
      process.env.HOME = home;
      const service = await loadService(baseline, scenario.name);
      add('resolveOpenClawModelConfig', [[state]], [service.resolveOpenClawModelConfig()]);
    }

    // -- extractor + factory ----------------------------------------------
    const extractor = await loadExtractor(baseline);
    const factory = await loadFactory(baseline);

    const headerInputs = [
      undefined, null, 'text', 7, [], [' a ', '', 3], {},
      {A: '  keep  ', B: '   ', C: 7, D: null, E: true},
      {__proto__: 'x', Ok: 'v'},
    ];
    for (const input of headerInputs) add('sanitizeHeaders', [input], extractor.sanitizeHeaders(input));

    const agentConfigs = [
      {agents: {defaults: {model: {primary: ' alpha/default '}}}},
      {agents: {defaults: {model: {primary: 'alpha/default'}}, worker: {model: {primary: ' worker/model '}}}},
      {agents: {defaults: {model: {primary: '   '}}, worker: {model: {primary: ''}}}},
      {agents: {defaults: {model: {primary: 7}}}},
      {agents: {defaults: {model: {primary: ['array']}}}},
      {agents: []},
      {},
    ];
    const agentIds = [undefined, 'worker', '', 'missing'];
    for (const config of agentConfigs) {
      for (const agentId of agentIds) {
        add('resolveAgentPrimaryModel', [config, agentId], extractor.resolveAgentPrimaryModel(config, agentId));
      }
    }

    const modelRefs = [
      undefined, '', ' ', 'gpt-5', ' alpha/model ', '/leading', 'alpha/', 'alpha//slash', 'alpha/a/b', '0/1',
    ];
    const modelConfigs = [
      {models: {providers: {alpha: {models: [{id: 'm1'}, {id: 'm2'}]}}}},
      {models: {providers: {only: {models: [{id: ''}, {id: 'm3'}]}}}},
      {models: {providers: {alpha: {}, beta: {models: [{id: 'b1'}]}}}},
      {models: {providers: {alpha: {models: [{id: 7}, {id: '  '}, {id: 'ok'}]}}}},
      {models: {providers: {alpha: {models: [null, 'x', {id: 'm'}]}}}},
      {models: {providers: {alpha: {models: []}}}},
      {models: {}},
      {},
    ];
    for (const config of modelConfigs) {
      for (const modelRef of modelRefs) {
        add('parseExtractorModelRef', [modelRef, config], extractor.parseExtractorModelRef(modelRef, config));
      }
    }

    for (const value of ['', 'A', 'A_B', 'A1', 'a', 'A-B', 'Ä', 'ABC ', '_', '0', 'A B']) {
      add('looksLikeEnvVarName', [value], extractor.looksLikeEnvVarName(value));
    }

    const selectionConfigs = [
      {models: {providers: {alpha: {api: 'anthropic', baseUrl: ' https://p.example/v1/// ', apiKey: 'k',
        headers: {P: ' p ', Blank: '  '}, models: [{id: 'm1', api: 'openai-completions', baseUrl: ' https://m.example// ',
        headers: {P: ' m ', M: ' mv '}}]}}}, agents: {defaults: {model: {primary: 'alpha/m1'}}}},
      {models: {providers: {alpha: {models: [{id: 'm1'}]}}}, agents: {defaults: {model: {primary: 'alpha/m1'}}}},
      {models: {providers: {alpha: {baseUrl: '   ', api: '', models: [{id: 'm1', baseUrl: '/'}]}}}, agents: {defaults: {model: {primary: 'alpha/m1'}}}},
      {models: {providers: {alpha: {models: [{id: 'm1'}]}}}, agents: {defaults: {model: {primary: 'alpha/missing'}}}},
      {models: {providers: {alpha: {models: []}}}},
      {models: {providers: {alpha: {models: [{id: 'm1', headers: [' arr ', '']}]}}}, agents: {defaults: {model: {primary: 'alpha/m1'}}}},
      {},
    ];
    for (const config of selectionConfigs) {
      for (const agentId of [undefined, 'worker']) {
        let expected: any;
        try {
          expected = extractor.__selection(config, agentId);
        } catch (error: any) {
          expected = {$error: error.message};
        }
        add('resolveSelection', [config, agentId], expected);
      }
    }

    const apiConfigs = [
      {models: {providers: {alpha: {apiKey: ' DIRECT_KEY '}}}},
      {models: {providers: {alpha: {apiKey: 'ENV_KEY'}}}},
      {models: {providers: {alpha: {apiKey: 'env_key'}}}},
      {models: {providers: {alpha: {apiKey: '  '}}}},
      {models: {providers: {alpha: {}}}},
      {models: {providers: {alpha: {apiKey: 7}}}},
      {},
    ];
    const apiEnvs = [{}, {ENV_KEY: ' from-env '}, {ENV_KEY: '   '}];
    const auths = [null, {$undefined: true}, {apiKey: ' auth '}, {apiKey: '   '}, {apiKey: 5}, {}];
    for (const config of apiConfigs) {
      for (const env of apiEnvs) {
        for (const auth of auths) {
          const runtime = auth === null ? undefined : {
            modelAuth: {resolveApiKeyForProvider: async () => (auth as any).$undefined ? undefined : auth},
          };
          let expected: any;
          await withEnv(env, async () => {
            try {
              expected = await extractor.__apiKey(config, 'alpha', runtime);
            } catch (error: any) {
              expected = {$error: error.message};
            }
          });
          add('resolveApiKey', [config, 'alpha', env, auth], expected);
        }
      }
    }

    const mergeInputs = [
      undefined, null, {}, {reasoningMode: 'accuracy_first'}, {reasoningMode: 'other'}, {reasoningMode: 7},
      {autoIndexIntervalMinutes: 5, autoDreamIntervalMinutes: 9},
      {autoIndexIntervalMinutes: -3.7, autoDreamIntervalMinutes: 2.9},
      {autoIndexIntervalMinutes: 0, autoDreamIntervalMinutes: 0},
      {autoIndexIntervalMinutes: '5', autoDreamIntervalMinutes: true},
      {autoIndexIntervalMinutes: NaN, autoDreamIntervalMinutes: Infinity},
      {autoIndexIntervalMinutes: -Infinity, autoDreamIntervalMinutes: 1e9},
    ];
    const service = await loadService(baseline, 'merge');
    for (const input of mergeInputs) add('mergeIndexingSettings', [input], service.mergeIndexingSettings(input));

    const memoryCfgs = [
      {model: 'alpha/m1'}, {model: 'alpha/a/b'}, {model: '/m'}, {model: 'm'}, {model: ''},
      {model: undefined, apiType: 'anthropic'}, {model: 'alpha/m1', apiType: 'google'},
      {model: 'alpha/m1', apiType: ''}, {model: 'epsilon/m'}, {model: 'missing/m'},
      {apiType: 'anthropic'},
    ];
    const modelConfig = {providers: {
      alpha: {protocol: 'openai', url: ' https://a.example ', apiKey: ' KEY '},
      beta: {protocol: 'anthropic'},
      gamma: {protocol: 'openai-responses'},
      delta: {protocol: 'google'},
      epsilon: {protocol: 'unknown'},
    }};
    for (const cfg of memoryCfgs) {
      for (const agentModel of [undefined, 'agent/model', '', 'no-slash']) {
        add('resolveMemoryLlm', [cfg, modelConfig, agentModel], factory.resolveMemoryLlm(cfg, modelConfig, agentModel));
      }
    }
    for (const protocol of [undefined, 'anthropic', 'google', 'openai-responses', 'openai', 'other', '']) {
      add('memoryApiTypeForProtocol', [protocol], factory.memoryApiTypeForProtocol(protocol));
    }
    // Extra boundary goldens. Appended after every existing case so the case
    // indices referenced by negative.py witnesses stay stable.
    const extraOpenClaw: any[] = [
      // `toStringRecord` rejects arrays (`isRecord` excludes them), so an
      // array-valued provider header record is dropped while the model object
      // headers still merge.
      {name: 'headers-array-provider', json: {
        models: {providers: {alpha: {
          headers: [' provider ', ''],
          models: [{id: 'm1', headers: {'X-Model': ' model '}}],
        }}},
        agents: {defaults: {model: {primary: 'alpha/m1'}}},
      }},
      // The mirror case: provider object headers merge, model array is dropped.
      {name: 'headers-array-model', json: {
        models: {providers: {alpha: {
          headers: {'X-Provider': ' provider '},
          models: [{id: 'm1', headers: [' model ', '']}],
        }}},
        agents: {defaults: {model: {primary: 'alpha/m1'}}},
      }},
      // JS own-key order enumerates integer keys ascending first. The raw
      // file text must keep `10` before `2` (JSON.stringify would normalize
      // it), so the fallback is only correct if the reader reorders integer
      // keys. The empty primary model also exercises the selected-model
      // fallback.
      {name: 'integer-providers-primary-miss', json: {
        models: {providers: {
          '10': {models: [{id: 'ten'}]},
          '2': {models: [{id: 'two'}]},
        }},
        agents: {defaults: {model: {primary: 'missing/'}}},
      }, text: '{"models":{"providers":{"10":{"models":[{"id":"ten"}]},"2":{"models":[{"id":"two"}]}}},"agents":{"defaults":{"model":{"primary":"missing/"}}}}'},
    ];
    for (const scenario of extraOpenClaw) {
      const home = join(scratch, `extra-openclaw-${index++}`);
      mkdirSync(home, {recursive: true});
      const state = scenario.text
        ? {kind: 'valid', json: scenario.json, text: scenario.text}
        : valid(scenario.json);
      writeOpenClaw(home, state);
      process.env.HOME = home;
      const service = await loadService(baseline, scenario.name);
      add('resolveOpenClawModelConfig', [[state]], [service.resolveOpenClawModelConfig()]);
    }
    // `resolveMemoryLlm` does not trim `cfg.model`: the space stays in the
    // provider/model halves and only an exact provider key hits `providers`.
    for (const model of [' alpha/m1 ', 'alpha/ m1 ']) {
      add('resolveMemoryLlm', [{model}, modelConfig, undefined],
          factory.resolveMemoryLlm({model}, modelConfig, undefined));
    }
  } finally {
    process.env.HOME = savedHome;
    rmSync(scratch, {recursive: true, force: true});
  }
  return {cases};
}
