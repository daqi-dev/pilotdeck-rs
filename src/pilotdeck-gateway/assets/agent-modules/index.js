import { createInterface } from "node:readline";

const PROTOCOL_VERSION = "2.0";

function nextId(prefix) {
  return `${prefix}-${Math.random().toString(36).slice(2)}`;
}

function defaultCapabilities() {
  return {
    capabilitiesVersion: "1",
    methods: [
      {
        name: "execute",
        enabled: true,
        profiles: ["streaming"],
        cancel: true,
        resumeSupport: "none",
        retry: "retry_after_status",
      },
      { name: "cancel", enabled: true },
      { name: "status", enabled: true },
      { name: "resume", enabled: false },
      { name: "ack", enabled: false },
    ],
  };
}

function writeLine(output, message) {
  return new Promise((resolve, reject) => {
    output.write(`${JSON.stringify(message)}\n`, (error) => {
      if (error) reject(error);
      else resolve();
    });
  });
}

function moduleOutcomeFromAgentResult(result) {
  if (!result || typeof result !== "object") return "failed";
  if (result.type === "success") return "success";
  if (result.type === "max_turns") return "max_turns";
  return "failed";
}

export class AgentLoopSidecarServer {
  constructor(factory, options = {}) {
    this.factory = factory;
    this.moduleId = options.moduleId ?? "pilotdeck-agent-loop";
    this.moduleInstanceId =
      options.moduleInstanceId ?? `${this.moduleId}-instance-${nextId("inst")}`;
    this.connectionGeneration = `${this.moduleInstanceId}-${nextId("gen")}`;
    this.abortControllers = new Map();
  }

  handshake(inReplyTo, method) {
    return {
      kind: "response",
      messageId: nextId(method),
      inReplyTo,
      ok: true,
      protocolVersion: PROTOCOL_VERSION,
      moduleId: this.moduleId,
      moduleInstanceId: this.moduleInstanceId,
      connectionGeneration: this.connectionGeneration,
      capabilitiesVersion: defaultCapabilities().capabilitiesVersion,
      payload: {},
    };
  }

  async handleExecute(request, output) {
    const active = this.abortControllers.get(request.operationId);
    if (active) {
      await writeLine(output, {
        kind: "response",
        messageId: nextId("execute-rejected"),
        inReplyTo: request.messageId,
        requestId: request.requestId,
        ok: false,
        code:
          active.runId === request.runId
            ? "OPERATION_ALREADY_ACTIVE"
            : "OPERATION_IDENTITY_MISMATCH",
      });
      return;
    }
    const controller = new AbortController();
    this.abortControllers.set(request.operationId, {
      runId: request.runId,
      requestId: request.requestId,
      controller,
    });
    const streamId = nextId("stream");
    let sequence = 0;
    try {
      await writeLine(output, {
        kind: "response",
        messageId: nextId("accepted"),
        inReplyTo: request.messageId,
        requestId: request.requestId,
        ok: true,
        streamId,
        cursor: 0,
      });
      const execution = await this.factory({
        request,
        abortSignal: controller.signal,
        abortExecution: (reason) => controller.abort(reason),
        callModule: async () => ({
          ok: false,
          code: "MODULE_CALL_UNSUPPORTED",
          error: { message: "Sidecar module_call is host-owned." },
        }),
      });
      if (execution?.loop?.run) {
        const iterator = execution.loop.run(execution.input);
        let next = await iterator.next();
        while (!next.done) {
          const event = next.value;
          await writeLine(output, {
            kind: "event",
            messageId: nextId("event"),
            eventType: `agent.${event?.type ?? "unknown"}`,
            streamId,
            sequence: sequence++,
            runId: request.runId,
            operationId: request.operationId,
            requestId: request.requestId,
            final: false,
            payload: event ?? {},
          });
          next = await iterator.next();
        }
        const result = next.value;
        await execution.flush?.();
        const outcome = moduleOutcomeFromAgentResult(result?.result);
        await writeLine(output, {
          kind: "response",
          messageId: nextId("final"),
          inReplyTo: request.messageId,
          requestId: request.requestId,
          ok: outcome === "success",
          final: true,
          outcome,
          streamId,
        });
      } else {
        await writeLine(output, {
          kind: "response",
          messageId: nextId("final"),
          inReplyTo: request.messageId,
          requestId: request.requestId,
          ok: true,
          final: true,
          outcome: "success",
          streamId,
        });
      }
    } catch (error) {
      await writeLine(output, {
        kind: "response",
        messageId: nextId("execute"),
        inReplyTo: request.messageId,
        requestId: request.requestId,
        ok: false,
        final: true,
        outcome: "failed",
        code: "EXECUTE_FAILED",
        error: { message: error?.message ?? String(error) },
      });
    } finally {
      this.abortControllers.delete(request.operationId);
    }
  }

  async serve(input = process.stdin, output = process.stdout) {
    const lines = createInterface({ input, crlfDelay: Infinity });
    try {
      for await (const line of lines) {
        if (!line.trim()) continue;
        let value;
        try {
          value = JSON.parse(line);
        } catch {
          await writeLine(output, {
            kind: "error",
            messageId: nextId("parse"),
            code: "INVALID_JSON",
            message: "Module message is not valid JSON.",
            retryability: "unsafe",
          });
          continue;
        }
        if (value?.kind !== "request") continue;
        if (value.method === "hello") {
          await writeLine(output, this.handshake(value.messageId, "hello"));
        } else if (value.method === "capabilities") {
          await writeLine(output, {
            ...this.handshake(value.messageId, "capabilities"),
            payload: defaultCapabilities(),
          });
        } else if (value.method === "execute") {
          await this.handleExecute(value, output);
        } else if (value.method === "cancel") {
          const active = this.abortControllers.get(value.operationId);
          const identityMatches = Boolean(
            active
              && active.runId === value.runId
              && (value.requestId === undefined || active.requestId === value.requestId),
          );
          if (identityMatches) {
            active.controller.abort(value.reason);
          }
          await writeLine(output, {
            kind: "response",
            messageId: nextId("cancel"),
            inReplyTo: value.messageId,
            requestId: value.requestId,
            ok: identityMatches,
            ...(identityMatches
              ? { payload: { operationId: value.operationId, cancelled: true } }
              : { code: active ? "OPERATION_IDENTITY_MISMATCH" : "UNKNOWN_OPERATION" }),
          });
        } else if (value.method === "status") {
          await writeLine(output, {
            kind: "response",
            messageId: nextId("status"),
            inReplyTo: value.messageId,
            requestId: value.requestId,
            ok: true,
            payload: { activeOperations: this.abortControllers.size },
          });
        }
      }
    } finally {
      lines.close();
    }
  }
}

export class AgentLoopSidecarTcpServer {
  constructor(sidecar) {
    this.sidecar = sidecar;
  }

  async listen(_options) {
    await new Promise(() => undefined);
  }
}
