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
// Own suspended frames instead of borrowing recursively. Children are restored
// before post-order callbacks and before returning Break, including mutations.
pub fn visit<'a>(item: &mut CollectionItem, visitor: &mut Visitor<'a>) {
    struct Frame<'a> {
        item: CollectionItem,
        control: Option<Control<'a>>,
        field: usize,
        index: usize,
        terminal: bool,
    }
    fn collection(item: &mut CollectionItem, field: usize) -> Option<&mut Token> {
        let token = match field {
            0 => item.key.as_mut().and_then(Option::as_mut),
            1 => item.value.as_mut(),
            _ => None,
        }?;
        if !token.is_collection() {
            return None;
        }
        Some(std::sync::Arc::make_mut(token))
    }
    let mut frames = vec![Frame {
        item: std::mem::take(item),
        control: None,
        field: 0,
        index: 0,
        terminal: false,
    }];
    let mut path = Vec::new();
    loop {
        let frame = frames.last_mut().unwrap();
        if frame.control.is_none() {
            frame.control = Some(visitor(&mut frame.item, &path));
            frame.terminal = matches!(
                frame.control,
                Some(Control::Break | Control::Skip | Control::Remove)
            );
        }
        let terminal = frame.terminal;
        if !terminal && frame.field < 2 {
            if let Some(token) = collection(&mut frame.item, frame.field) {
                if frame.index < token.items.len() {
                    let child = std::mem::take(&mut token.items[frame.index]);
                    path.push((
                        if frame.field == 0 {
                            Field::Key
                        } else {
                            Field::Value
                        },
                        frame.index,
                    ));
                    frames.push(Frame {
                        item: child,
                        control: None,
                        field: 0,
                        index: 0,
                        terminal: false,
                    });
                    continue;
                }
                if frame.field == 0 && matches!(frame.control, Some(Control::Next(_))) {
                    let Some(Control::Next(mut next)) = frame.control.take() else {
                        unreachable!()
                    };
                    frame.control = Some(next(&mut frame.item, &path));
                }
            }
            frame.field += 1;
            frame.index = 0;
            continue;
        }
        let mut done = frames.pop().unwrap();
        let control = match done.control.take().unwrap() {
            Control::Next(mut next) if !terminal => next(&mut done.item, &path),
            other => other,
        };
        let Some(parent) = frames.last_mut() else {
            *item = done.item;
            return;
        };
        path.pop();
        let token = collection(&mut parent.item, parent.field).unwrap();
        token.items[parent.index] = done.item;
        match control {
            Control::Remove => {
                token.items.remove(parent.index);
            }
            Control::Index(next) => parent.index = next,
            Control::Break => {
                // Unwind every suspended owner while restoring the original
                // structure. No recursive drops and no lost child mutations.
                while let Some(done) = frames.pop() {
                    if let Some(parent) = frames.last_mut() {
                        collection(&mut parent.item, parent.field).unwrap().items[parent.index] =
                            done.item;
                    } else {
                        *item = done.item;
                    }
                }
                return;
            }
            _ => parent.index += 1,
        }
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
