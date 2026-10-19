import fs from 'node:fs/promises';
import path from 'node:path';
export async function prepareFixture(scratch) {
  const project=path.join(scratch,'project'),pilot=path.join(scratch,'pilot');
  await fs.mkdir(path.join(pilot,'projects','registered','chats'),{recursive:true});
  await fs.mkdir(project,{recursive:true});
  await fs.writeFile(path.join(pilot,'projects','registered','.cwd'),project);
  await fs.writeFile(path.join(pilot,'pilotdeck.yaml'),`schemaVersion: 1
agent: {model: test/model, maxContextTokens: 65536, maxOutputTokens: 8192}
model:
  providers:
    test:
      protocol: openai
      url: https://example.test/v1
      apiKey: fixture-only
      models: {model: {}}
extension:
  builtinPluginsEnabled: {windows-skills: false, browser-use: false, funasr: false}
telemetry: {enabled: false}
`);
  process.chdir(project);process.env.PILOT_HOME=pilot;
  return {project,pilot};
}
