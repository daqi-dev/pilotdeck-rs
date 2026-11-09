# CLI 候选补丁合并

候选的 base 必须是生成内容时保存的原字节。只把 manifest 更新为新 canonical 的 hash，会让旧候选合法地删除另一车道刚加的代码；2026-11-08 runtime discovery 候选因此暂时删掉 `mod web`，编译检出后恢复。已冻结 patch/manifest 只读，新增修复写独立目录或增量文件。协调者除了核 base/patch/target hash，还须逐 hunk 核共享文件的非本片上下文；`git apply --check` 不证明没有逻辑覆盖。

本仓 `JsValue` 只实现 `From<&str>`，不实现 bool、number、String 的 From；用 Bool/Number/String 显式variant，或 String.as_str().into()。`JsValue` 有 Drop，match 按引用取 Object 后 clone，不能移出内部对象。新测试与产品代码同样遵守，避免反复编译才发现这些类型错误。
