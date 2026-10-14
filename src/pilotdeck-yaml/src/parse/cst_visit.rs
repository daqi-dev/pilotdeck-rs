// Port of yaml@2.9.0 dist/parse/cst-visit.js (ISC).
use super::cst::{CollectionItem, Token};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Key,
    Value,
}
pub type Path = Vec<(Field, usize)>;
pub type Visitor<'a> = dyn FnMut(&mut CollectionItem, &[(Field, usize)]) -> Control<'a> + 'a;
pub enum Control<'a> {
    Continue,
    Break,
    Skip,
    Remove,
    Index(usize),
    Next(Box<Visitor<'a>>),
}
pub fn visit<'a>(item: &mut CollectionItem, visitor: &mut Visitor<'a>) {
    visit_item(&mut Vec::new(), item, visitor);
}
fn visit_item<'a>(
    path: &mut Path,
    item: &mut CollectionItem,
    visitor: &mut Visitor<'a>,
) -> Control<'a> {
    let mut ctrl = visitor(item, path);
    if matches!(ctrl, Control::Break | Control::Skip | Control::Remove) {
        return ctrl;
    }
    for field in [Field::Key, Field::Value] {
        let token = match field {
            Field::Key => item.key.as_mut().and_then(Option::as_deref_mut),
            Field::Value => item.value.as_deref_mut(),
        };
        if let Some(token) = token.filter(|t| t.is_collection()) {
            let mut i = 0;
            while i < token.items.len() {
                path.push((field, i));
                let ci = visit_item(path, &mut token.items[i], visitor);
                path.pop();
                match ci {
                    Control::Index(next) => i = next,
                    Control::Break => return Control::Break,
                    Control::Remove => {
                        token.items.remove(i);
                    }
                    _ => i += 1,
                }
            }
            if field == Field::Key
                && let Control::Next(mut next) = ctrl
            {
                ctrl = next(item, path);
            }
        }
    }
    if let Control::Next(mut next) = ctrl {
        next(item, path)
    } else {
        ctrl
    }
}
pub fn item_at_path<'a>(
    root: &'a CollectionItem,
    path: &[(Field, usize)],
) -> Option<&'a CollectionItem> {
    let mut item = root;
    for (field, index) in path {
        let token = match field {
            Field::Key => item.key.as_ref()?.as_deref()?,
            Field::Value => item.value.as_deref()?,
        };
        if !token.is_collection() {
            return None;
        }
        item = token.items.get(*index)?;
    }
    Some(item)
}
pub fn parent_collection<'a>(
    root: &'a CollectionItem,
    path: &[(Field, usize)],
) -> Result<&'a Token, &'static str> {
    let Some(((field, _), parent_path)) = path.split_last() else {
        return Err("Parent collection not found");
    };
    let parent = item_at_path(root, parent_path).ok_or("Parent collection not found")?;
    let token = match field {
        Field::Key => parent.key.as_ref().and_then(|t| t.as_deref()),
        Field::Value => parent.value.as_deref(),
    };
    token
        .filter(|t| t.is_collection())
        .ok_or("Parent collection not found")
}
