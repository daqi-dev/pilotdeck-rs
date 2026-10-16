---
name: test-oracles
description: 宿主能力前提必须由独立原生探针判断，不能让被测包装替自己证明未跳过。
type: feedback
---

#120权限夹具先只认root，再拓宽为被测entries返回Ok；两者都不能独立证明宿主无法制造EACCES，后者还会吞掉未来包装退化。第三次修复后固化此规则，避免围着同一护栏逐条补洞。

标真实EACCES不可达前，先用独立std::fs::read_dir并完整collect，和被测NativeFiles::entries交叉核对：原生报EACCES而包装Ok必须FAIL，两者Err核具体错误，两者Ok才记宿主能力不可达；录制错误replay仍必跑。用“包装吞错为Ok([])”变异负控证明不能假通过，反向不一致与错误probe errno同样拒绝。不可达记录不是实际权限验收PASS，未知FUSE/root不能被单元模拟代填。
