# F1 storage 键盘点（静态，非运行时穷举）

扫描 ui/src 的 769 个 TS/JS 文件；197 个调用点（生产 125）。直接调用与简单 storage 别名全留档；函数封装、动态模板和第三方调用仍需 F2 网络/运行时录制。

| area | 已解析键 | 依据（baseline ui/） |
|---|---|---|
| localStorage | `PILOTDECK_GITHUB_STARS` | src/hooks/useGitHubStars.ts:27 |
| localStorage | `PILOTDECK_HIDE_GITHUB_STAR` | src/hooks/useGitHubStars.ts:16 |
| localStorage | `activeTab` | src/hooks/useProjectsState.ts:255 |
| localStorage | `auth-token` | src/components/auth/context/AuthContext.tsx:21 |
| localStorage | `codeEditorFontSize` | src/components/settings/shared/hooks/useSettingsController.ts:41 |
| localStorage | `codeEditorLineNumbers` | src/components/settings/shared/hooks/useSettingsController.ts:39 |
| localStorage | `codeEditorShowMinimap` | src/components/settings/shared/hooks/useSettingsController.ts:38 |
| localStorage | `codeEditorTheme` | src/components/settings/shared/hooks/useSettingsController.ts:70 |
| localStorage | `codeEditorWordWrap` | src/components/settings/shared/hooks/useSettingsController.ts:37 |
| localStorage | `pilotdeck-model` | src/components/chat/hooks/useChatProviderState.ts:104 |
| localStorage | `pilotdeck-settings` | src/components/settings/shared/hooks/useSettingsController.ts:63 |
| localStorage | `pilotdeck:dashboard-panel-width` | src/components/main-content/view/MainContent.tsx:175 |
| localStorage | `pilotdeck:files-assistant-width` | src/components/main-content/view/MainContent.tsx:161 |
| localStorage | `pilotdeck:files-panel-layout` | src/components/main-content/view/MainContent.tsx:85 |
| localStorage | `pilotdeck:files-panel-order` | src/components/main-content/view/MainContent.tsx:95 |
| localStorage | `sidebar-v2-projects-conversations-split` | src/components/app-shell/SidebarV2.tsx:317 |
| localStorage | `sidebar-v2-projects-conversations-split-v2` | src/components/app-shell/SidebarV2.tsx:316 |
| localStorage | `sidebar-v2-width` | src/components/app-shell/SidebarV2.tsx:338 |
| localStorage | `tasks-enabled` | src/contexts/TasksSettingsContext.jsx:25 |
| localStorage | `theme` | src/contexts/ThemeContext.jsx:11 |
| localStorage | `themeMode` | src/contexts/ThemeContext.jsx:11 |
| localStorage | `ultrarag_enterprise_agent_scope` | src/composition/modules/staffdeck/vendor/FormalHostContractHelpers.ts:16 |
| localStorage | `userLanguage` | src/i18n/config.js:53 |
| sessionStorage | `pendingSessionId` | src/components/chat/hooks/useChatComposerState.ts:1282 |
| sessionStorage | `pilotdeck-active-project` | src/components/app-shell/AppShellV2.tsx:76 |
| sessionStorage | `pilotdeck:ui-diagnostics` | src/lib/uiDiagnostics.ts:10 |

生产动态调用 38 处（完整表达式、源码路径、行号见 evidence/storage.json）；不得把表达式当作已穷举的键。

| 动态键表达式 | area | 依据 |
|---|---|---|
| `key` | sessionStorage | src/components/chat-v2/SopWaitBanner.tsx:200 |
| `key` | sessionStorage | src/components/chat-v2/SopWaitBanner.tsx:206 |
| `key` | sessionStorage | src/components/chat-v2/SopWaitBanner.tsx:207 |
| `key` | localStorage | src/components/chat/utils/chatStorage.ts:14 |
| `k` | localStorage | src/components/chat/utils/chatStorage.ts:22 |
| `key` | localStorage | src/components/chat/utils/chatStorage.ts:26 |
| `key` | localStorage | src/components/chat/utils/chatStorage.ts:37 |
| `key` | localStorage | src/components/chat/utils/chatStorage.ts:45 |
| `storageKey` | localStorage | src/components/code-editor/hooks/useCodeEditorSettings.ts:10 |
| `CODE_EDITOR_STORAGE_KEYS.wordWrap` | localStorage | src/components/code-editor/hooks/useCodeEditorSettings.ts:19 |
| `CODE_EDITOR_STORAGE_KEYS.fontSize` | localStorage | src/components/code-editor/hooks/useCodeEditorSettings.ts:23 |
| `CODE_EDITOR_STORAGE_KEYS.theme` | localStorage | src/components/code-editor/hooks/useCodeEditorSettings.ts:46 |
| `CODE_EDITOR_STORAGE_KEYS.wordWrap` | localStorage | src/components/code-editor/hooks/useCodeEditorSettings.ts:50 |
| `key` | localStorage | src/components/project-creation-wizard/components/FolderBrowserModal.tsx:23 |
| `storageKey` | localStorage | src/components/project-creation-wizard/components/FolderBrowserModal.tsx:67 |
| ``${DISTILL_REWRITE_MODEL_STORAGE_KEY}:${TENANT_ID}`` | localStorage | src/composition/modules/staffdeck/vendor/DistillPage.tsx:619 |
| ``${DISTILL_REWRITE_MODEL_STORAGE_KEY}:${TENANT_ID}`` | localStorage | src/composition/modules/staffdeck/vendor/DistillPage.tsx:879 |
| ``${DISTILL_REWRITE_MODEL_STORAGE_KEY}:${TENANT_ID}`` | localStorage | src/composition/modules/staffdeck/vendor/DistillPage.tsx:2568 |
| `key` | sessionStorage | src/composition/modules/staffdeck/vendor/DistillPage.tsx:7824 |
| `key` | sessionStorage | src/composition/modules/staffdeck/vendor/DistillPage.tsx:7875 |
| `key` | sessionStorage | src/composition/modules/staffdeck/vendor/DistillPage.tsx:7889 |
| `key` | sessionStorage | src/composition/modules/staffdeck/vendor/DistillPage.tsx:7897 |
| ``${KNOWLEDGE_SEARCH_MODEL_STORAGE_KEY}:${TENANT_ID}`` | localStorage | src/composition/modules/staffdeck/vendor/KnowledgePage.tsx:150 |
| ``${KNOWLEDGE_SEARCH_MODEL_STORAGE_KEY}:${TENANT_ID}`` | localStorage | src/composition/modules/staffdeck/vendor/KnowledgePage.tsx:259 |
| ``${KNOWLEDGE_SEARCH_MODEL_STORAGE_KEY}:${TENANT_ID}`` | localStorage | src/composition/modules/staffdeck/vendor/KnowledgePage.tsx:1242 |
| `AGENT_SCOPE_KEY` | localStorage | src/composition/modules/staffdeck/vendor/copy-scope.ts:14 |
| `AGENT_SCOPE_KEY` | localStorage | src/composition/modules/staffdeck/vendor/copy-scope.ts:48 |
| `LIGHT_APPEARANCE_KEY` | localStorage | src/contexts/ThemeContext.jsx:44 |
| `INTERFACE_PREFERENCES_KEY` | localStorage | src/hooks/useInterfacePreferences.ts:7 |
| `INTERFACE_PREFERENCES_KEY` | localStorage | src/hooks/useInterfacePreferences.ts:24 |
| `key` | localStorage | src/hooks/useLocalStorage.jsx:16 |
| `key` | localStorage | src/hooks/useLocalStorage.jsx:31 |
| `key` | localStorage | src/hooks/useUiPreferences.ts:69 |
| `storageKey` | localStorage | src/hooks/useUiPreferences.ts:86 |
| `storageKey` | localStorage | src/hooks/useUiPreferences.ts:162 |
| `LIGHT_APPEARANCE_KEY` | localStorage | src/lib/appearanceRuntime.ts:7 |
| `key` | localStorage | src/lib/customNames.ts:27 |
| `key` | localStorage | src/lib/customNames.ts:37 |

共享契约最少包括 auth-token、sessionStorage 的 pilotdeck-active-project、draft_input_<projectName>:<sessionId|new>、themeMode/theme、userLanguage。i18next detector 默认 i18nextLng 是第三方隐含键，须 F2 录制复核；不是本 AST 扫描的直接调用。实验只读取源码，不读取用户浏览器 storage。
