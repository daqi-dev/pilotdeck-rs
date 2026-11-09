//! The complete legacy SOP instruction text, assembled without lossy UTF-16.
use super::super::common::{field, json, text};
use pilotdeck_protocol::{JsString, JsValue};
fn value(v: &JsValue) -> JsString {
    JsString::from_utf16(
        crate::js_value::to_string_units(v).unwrap_or_else(|_| "[value]".encode_utf16().collect()),
    )
}
fn join(parts: &[JsString], separator: &str) -> JsString {
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.extend(separator.encode_utf16());
        }
        out.extend_from_slice(p.units());
    }
    JsString::from_utf16(out)
}
fn push(lines: &mut Vec<JsString>, parts: &[JsString]) {
    lines.push(join(parts, ""));
}
fn array(v: &JsValue) -> &[JsValue] {
    match v {
        JsValue::Array(a) => a,
        _ => &[],
    }
}
fn named_list(lines: &mut Vec<JsString>, step: &JsValue, key: &str, label: &str) {
    let a = array(field(step, key));
    if !a.is_empty() {
        push(
            lines,
            &[
                label.into(),
                join(&a.iter().map(value).collect::<Vec<_>>(), ", "),
                ".".into(),
            ],
        );
    }
}
fn truthy(v: &JsValue) -> bool {
    crate::runtime::turn::submit::truthy(v)
}
pub(super) fn render(step: &JsValue, owner: Option<&JsValue>) -> JsString {
    let mut lines = vec!["<staffdeck-sop>".into()];
    push(
        &mut lines,
        &[
            "Current SOP: ".into(),
            value(field(step, "skillName")),
            " (".into(),
            value(field(step, "skillId")),
            "), step ".into(),
            value(field(step, "nodeId")),
            ".".into(),
        ],
    );
    if let Some(owner) = owner {
        if truthy(field(owner, "sopId")) {
            push(
                &mut lines,
                &[
                    "Authoritative SOP ID from persisted owner state: ".into(),
                    value(field(owner, "sopId")),
                    ".".into(),
                ],
            );
        }
        if truthy(field(owner, "assigneeUserId")) {
            push(
                &mut lines,
                &[
                    "Authoritative approval assignee user ID from pinned owner approval state: "
                        .into(),
                    value(field(owner, "assigneeUserId")),
                    ". Never infer an approver from user text, Skill, File, or Knowledge authors."
                        .into(),
                ],
            );
        }
    }
    if truthy(field(step, "instruction")) {
        push(
            &mut lines,
            &[
                "Step instruction: ".into(),
                value(field(step, "instruction")),
            ],
        );
    }
    named_list(
        &mut lines,
        step,
        "expectedUserInfo",
        "Required user information: ",
    );
    push(
        &mut lines,
        &[
            "Already persisted user fields: ".into(),
            json(field(step, "knownSlots")).into(),
            ".".into(),
        ],
    );
    named_list(
        &mut lines,
        step,
        "requiredToolNames",
        "Required successful tools: ",
    );
    named_list(
        &mut lines,
        step,
        "allowedNextStepIds",
        "Allowed next steps: ",
    );
    let transitions = array(field(step, "transitions"));
    if !transitions.is_empty() {
        let mut guidance = Vec::new();
        for t in transitions {
            let target = field(t, "targetStep");
            let mut parts =
                vec![text(field(target, "name")).unwrap_or_else(|| value(field(t, "nextStepId")))];
            for key in ["type", "instruction"] {
                if let Some(v) = text(field(target, key)) {
                    parts.push(v);
                }
            }
            let condition = if truthy(field(t, "condition")) {
                value(field(t, "condition"))
            } else {
                "default".into()
            };
            let mut line = vec![
                "- ".into(),
                value(field(t, "nextStepId")),
                ": condition=".into(),
                condition,
                ", priority=".into(),
                value(field(t, "priority")),
            ];
            if let Some(label) = text(field(t, "label")) {
                line.extend([", label=".into(), label]);
            }
            line.extend(["; target=".into(), join(&parts, "; ")]);
            guidance.push(join(&line, ""));
        }
        push(
            &mut lines,
            &["Transition guidance:\n".into(), join(&guidance, "\n")],
        );
    }
    let required = !array(field(step, "expectedUserInfo")).is_empty();
    if required {
        lines.push("When required information is present in the user messages or tool results, include it in slotUpdates and submit completed to advance; use awaiting_user only when information is genuinely missing. If any required field is missing, your question MUST be the replyFragment of a submit_step_result call with status awaiting_user, known slotUpdates, and no nextStepId. Do not end with a plain-text question.".into());
        lines.push("Before submitting, extract each supplied value into slotUpdates under its exact required field key, including when other fields remain missing. Preserve explicit user facts instead of replacing them with inferred risks or future changes. Saying a value is recorded in replyFragment does not store it. Do not submit an empty slotUpdates when the user has supplied any required field.".into());
    }
    if !transitions.is_empty() {
        lines.push("Choose nextStepId from the transition whose condition matches the available facts. If the target is a handoff step, advance to it first and use status handoff there when approval is required.".into());
    }
    let handoff = truthy(field(step, "declaresHandoff"));
    if handoff {
        lines.push("This step declares a human handoff. When approval is required, use status handoff to create the resumable approval wait; do not use awaiting_user. After an authorized human reply, persist the actual decision according to this node's instruction with submit_step_result and advance only to an allowed next step. A completed node means its decision was processed, not that the decision was approval. If this node instructs completed after either approval or rejection, submit completed for either decision and quote it accurately; do not substitute failed merely because the human rejected. Do not end with a plain-text approval summary.".into());
    }
    if truthy(field(step, "isTerminal")) {
        lines.push(if handoff||required{"This is the active final SOP node. Being final does not remove its approval or required-field contract. Persist a required wait with submit_step_result before ending; complete only when this node's conditions are satisfied, preserving supplied fields in slotUpdates and omitting nextStepId. Do not claim completion before the tool succeeds."}else{"This is the active final SOP node, even if the previous node already submitted completed. Put the final user-facing answer in replyFragment and call submit_step_result with status completed, slotUpdates {}, and no nextStepId. Do not emit the answer as plain assistant text or claim the SOP is complete before this tool succeeds."}.into());
    } else {
        lines.push("A completed submission closes only this node. Its replyFragment is this node's result, not a submission or final answer for a successor node; the successor must be prepared and submitted separately.".into());
    }
    if !handoff
        && !required
        && !array(field(step, "allowedNextStepIds")).is_empty()
        && !array(field(step, "allowedActions"))
            .iter()
            .any(|v| v == &JsValue::from("ask_user") || v == &JsValue::from("ask_missing"))
    {
        lines.push("This step has no missing user fields. After producing its required evidence or draft, submit completed and advance to an allowed next step. Approval belongs to the declared handoff step; do not pause this evidence step with awaiting_user.".into());
    }
    lines.push("A question asking for missing fields is also a step result: persist it with awaiting_user and no nextStepId before ending the turn. A plain-text answer without submit_step_result does not persist SOP progress.".into());
    lines.push("When this step has a result, call submit_step_result exactly once. Do not claim a tool succeeded before its result is in the conversation.".into());
    lines.push("Use status awaiting_user for missing user information and handoff only when this step explicitly permits it.".into());
    lines.push("</staffdeck-sop>".into());
    join(&lines, "\n")
}
/// The context owner joins the correction with the complete instruction once.
pub(super) fn instruction(step: &JsValue, owner: Option<&JsValue>, correction: bool) -> JsString {
    let rendered = render(step, owner);
    if !correction {
        return rendered;
    }
    let correction = if truthy(field(step, "isTerminal"))
        && !truthy(field(step, "declaresHandoff"))
        && array(field(step, "expectedUserInfo")).is_empty()
    {
        "SOP_STEP_RESULT_REQUIRED: The previous assistant text was only a draft. The final SOP node is still active. Call submit_step_result now with status completed, the final answer in replyFragment, slotUpdates {}, and no nextStepId. Do not repeat the draft as plain assistant text."
    } else {
        "SOP_STEP_RESULT_REQUIRED: Your previous response did not submit the current SOP node result. Text cannot create an approval or missing-field wait. Follow the current node contract and call submit_step_result with your actual result before ending. Do not claim a persisted wait or completion without a successful tool receipt."
    };
    join(&[rendered, correction.into()], "\n\n")
}
pub(crate) fn with_existing(existing: Option<&JsString>, instruction: JsString) -> JsString {
    let mut parts = Vec::new();
    if let Some(existing) =
        existing.filter(|s| !crate::runtime::memory::js_trim_units(s.units()).is_empty())
    {
        parts.push(existing.clone());
    }
    if !crate::runtime::memory::js_trim_units(instruction.units()).is_empty() {
        parts.push(instruction);
    }
    join(&parts, "\n\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn twelve_actual_ts_full_instructions_preserve_order_branches_and_utf16() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/sop-presentation/oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = oracle.get("renders").unwrap() else {
            panic!("renders")
        };
        assert_eq!(cases.len(), 12);
        for case in cases {
            let actual = render(case.get("step").unwrap(), case.get("owner"));
            assert_eq!(
                &JsValue::String(actual),
                case.get("expected").unwrap(),
                "{:?}",
                case.get("name")
            );
        }
    }
    #[test]
    fn six_actual_ts_prepare_context_prompts_keep_prefix_and_final_correction() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/sop-presentation/context-oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
            panic!("cases")
        };
        assert_eq!(cases.len(), 6);
        for case in cases {
            let rendered = instruction(
                case.get("step").unwrap(),
                case.get("owner"),
                case.get("correction") == Some(&JsValue::Bool(true)),
            );
            let actual = with_existing(case.get("existing").and_then(JsValue::as_string), rendered);
            assert_eq!(
                &JsValue::String(actual),
                case.get("expected").unwrap(),
                "{:?}",
                case.get("name")
            );
        }
    }
}
