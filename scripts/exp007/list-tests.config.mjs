// Inventory only (--list): never use this config to launch a browser.
import {baseline,requireUI,root} from './env.mjs';
const {defineConfig}=requireUI('@playwright/test');
export default defineConfig({testDir:`${baseline}/e2e`,testMatch:'**/*.spec.mjs',forbidOnly:true,workers:1,outputDir:`${root}/_tmp/exp007/playwright-output`,reporter:'json'});
