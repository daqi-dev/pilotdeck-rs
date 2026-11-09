# WeCom TS 本地金样

固定 OpenBMB/PilotDeck cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0、ICU 78.2、Darwin arm64。WeComChannel.ts SHA256 516a04715f9872f44f5d823a5deb254d1b9e025d63e03c681c4b64717e5ac1f1；WeComSessionMapper.ts 94a026c339d184be7c4fbb41f26cff1b8e7c054413214a9178013c2a0e3362bc；wecom-render.ts 6e2f66ad321240e2f7d38d9f3b85a1a7ae737c38b8bf76869833491708f6e7ce。

oracle.json 是真实 mapper/render 输出；wire.json 是 fake WebSocket 经真实 WeComChannel.start、订阅、回调、假 gateway、回复与重复消息的输出；incoming.json 是 mixed/group mention/voice/quote 经真实 channel 送至假 gateway 的输出。fixture 中 bot/secret 均为明确的假值，不连接网络。原始采集与 stderr 位于主仓 _tmp/20261110-wecom-ts/，未覆盖。

复采：PDRS_BASELINE=/path/to/fixed-baseline node --import /path/to/fixed-baseline/node_modules/tsx/dist/loader.mjs capture.mts （以及 wire-capture.mts、incoming-capture.mts）；输出和 stderr 由调用方写入全新独占目录。当前仅覆盖纯函数和 mock 消息，不证明网络、附件、完整 channel 或整阶段通过。
