//! WeCom session scoping from fixed WeComSessionMapper.ts.
use pilotdeck_protocol::{JsObject, JsString, JsValue};

#[derive(Clone, Default)]
pub(crate) struct WeComSessionMapper {
    active: JsObject,
    projects: JsObject,
}

fn trim(value: &JsString) -> JsString {
    JsString::from_utf16(crate::runtime::memory::js_trim_units(value.units()).to_vec())
}
fn input_string(value: &JsValue, key: &str) -> Option<JsString> {
    value.get(key).and_then(JsValue::as_string).map(trim)
}
fn append(out: &mut Vec<u16>, value: &str) {
    out.extend(value.encode_utf16());
}

impl WeComSessionMapper {
    pub(crate) fn from_state(state: &JsValue) -> Self {
        Self {
            active: state
                .get("activeByChatId")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default(),
            projects: state
                .get("projectByScopeKey")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default(),
        }
    }
    fn scope(input: &JsValue) -> Result<JsString, &'static str> {
        let chat = input_string(input, "chatId").ok_or("chatId must be a string")?;
        let user = match input.get("userId") {
            None | Some(JsValue::Undefined | JsValue::Null) => None,
            Some(JsValue::String(user)) => Some(trim(user)).filter(|s| !s.units().is_empty()),
            Some(_) => return Err("userId must be a string"),
        };
        let group = input
            .get("chatType")
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("group"));
        let mut out = Vec::new();
        if group {
            append(&mut out, "wecom:group=");
            out.extend_from_slice(chat.units());
            if input.get("groupSessionsPerUser") != Some(&JsValue::Bool(false))
                && let Some(user) = user
            {
                append(&mut out, ":user=");
                out.extend_from_slice(user.units());
            }
        } else {
            append(&mut out, "wecom:dm=");
            out.extend_from_slice(user.as_ref().unwrap_or(&chat).units());
        }
        Ok(JsString::from_utf16(out))
    }
    pub(crate) fn resolve(
        &mut self,
        input: &JsValue,
        uuid: &JsString,
    ) -> Result<JsValue, &'static str> {
        let key = Self::scope(input)?;
        let message = input_string(input, "text").ok_or("text must be a string")?;
        let units = message.units();
        let is_new = units == "/new".encode_utf16().collect::<Vec<_>>()
            || units.starts_with(&['/' as u16, 'n' as u16, 'e' as u16, 'w' as u16, ' ' as u16]);
        let mut out = JsObject::new();
        let mut session = key.units().to_vec();
        if is_new {
            append(&mut session, ":s_");
            session.extend_from_slice(uuid.units());
            self.active.insert(
                key.clone(),
                JsValue::String(JsString::from_utf16(session.clone())),
            );
        } else if let Some(existing) = self.active.get_key(&key).and_then(JsValue::as_string) {
            session = existing.units().to_vec();
        } else {
            append(&mut session, ":general");
        }
        out.insert(
            "sessionKey".into(),
            JsValue::String(JsString::from_utf16(session)),
        );
        if let Some(project) = self.projects.get_key(&key) {
            out.insert("projectKey".into(), project.clone());
        }
        if is_new {
            out.insert("command".into(), "new".into());
        }
        let content = if is_new {
            crate::runtime::memory::js_trim_units(&units[4..])
        } else {
            units
        };
        out.insert(
            "message".into(),
            JsValue::String(JsString::from_utf16(content.to_vec())),
        );
        Ok(JsValue::Object(out))
    }
    pub(crate) fn bind_project(
        &mut self,
        input: &JsValue,
        project: JsString,
    ) -> Result<(), &'static str> {
        self.projects
            .insert(Self::scope(input)?, JsValue::String(project));
        Ok(())
    }
    pub(crate) fn get_project(&self, input: &JsValue) -> Result<Option<JsString>, &'static str> {
        Ok(self
            .projects
            .get_key(&Self::scope(input)?)
            .and_then(JsValue::as_string)
            .cloned())
    }
    pub(crate) fn snapshot(&self) -> JsValue {
        let mut result = JsObject::new();
        result.insert(
            "activeByChatId".into(),
            JsValue::Object(self.active.clone()),
        );
        result.insert(
            "projectByScopeKey".into(),
            JsValue::Object(self.projects.clone()),
        );
        JsValue::Object(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn mapper_matches_fixed_typescript_oracle() {
        let oracle = parse_json(include_str!("fixtures/oracle.json")).unwrap();
        let JsValue::Array(inputs) = oracle.get("inputs").unwrap() else {
            panic!("inputs")
        };
        let JsValue::Array(results) = oracle.get("results").unwrap() else {
            panic!("results")
        };
        let mut mapper = WeComSessionMapper::default();
        let uuid = JsString::from("00000000-0000-4000-8000-000000000001");
        for (input, expected) in inputs.iter().zip(results) {
            assert_eq!(&mapper.resolve(input, &uuid).unwrap(), expected);
        }
        let group =
            parse_json(r#"{"chatId":"room","userId":"alice","chatType":"group","text":"bound"}"#)
                .unwrap();
        mapper.bind_project(&group, "/project".into()).unwrap();
        assert_eq!(mapper.resolve(&group, &uuid).unwrap(), results[6]);
        assert_eq!(
            mapper.get_project(&group).unwrap(),
            Some(JsString::from("/project"))
        );
        assert_eq!(&mapper.snapshot(), oracle.get("state").unwrap());
    }
}
