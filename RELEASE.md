# GitHub 自动发布指南

PromLex 使用 GitHub Actions 自动发布正式版本。将 `releases/vX.Y.Z` 格式的 tag 推送到 GitHub 后，工作流会检查版本和代码、构建各平台安装包，并在全部成功后公开 GitHub Release。

日常发布无需本地打包，也无需手动上传安装包。

## 1. 相关文件

| 文件 | 用途 |
| --- | --- |
| [`.github/workflows/release.yml`](.github/workflows/release.yml) | tag 触发、版本检查、跨平台构建和发布 |
| [`scripts/check-release-version.mjs`](scripts/check-release-version.mjs) | 检查 tag 格式与项目版本号 |
| [`scripts/bump-version.mjs`](scripts/bump-version.mjs) | 同步更新 npm、Cargo、Tauri 版本号 |
| [`.github/workflows/build.yml`](.github/workflows/build.yml) | 普通 push 和 pull request 的构建检查 |

两个工作流独立运行；发布所需的检查在 `Release` 工作流内执行，发布任务直接依赖这些检查与构建结果。

## 2. 首次启用

1. 将发布工作流、版本校验脚本和文档提交并推送到 GitHub。
2. 确认仓库启用了 Actions，仓库或组织策略允许运行工作流中使用的 Actions。
3. 确认策略允许发布任务请求 `contents: write`，用于创建 Release 和上传附件。
4. 创建 tag 时，确保它指向包含发布工作流的提交。

工作流使用 GitHub 自动提供的 `GITHUB_TOKEN`，无需创建个人访问令牌，也无需手动添加同名 Secret。构建任务使用只读仓库权限，只有最终发布任务请求写权限。

## 3. 日常发布

以下命令在仓库根目录执行，以发布 `1.0.2` 为例。将示例版本替换为本次实际版本。

### 更新并检查版本

先完成代码修改和必要验证，再同步更新版本号：

```powershell
npm run version:bump -- 1.0.2
node scripts/check-release-version.mjs releases/v1.0.2
```

版本脚本默认只修改元数据，不在本地编译。也可使用 `npm run version:bump -- patch` 自动增加补丁版本，再用实际版本执行校验。

校验要求以下位置的版本全部与 tag 一致：

- `package.json` 的 `version`。
- `package-lock.json` 的顶层 `version` 和 `packages[""].version`。
- `src-tauri/Cargo.toml` 中应用包的版本。
- `src-tauri/Cargo.lock` 中 `promlex` 包的版本。
- `src-tauri/tauri.conf.json` 的 `version`。

### 提交并推送代码

确保本次功能修改也已提交。以下命令单独提交版本文件：

```powershell
git add package.json package-lock.json src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json
git commit -m "Release 1.0.2"
git push origin HEAD
```

首次发布前，新增的工作流和校验脚本也必须已经提交。

### 创建并推送 tag

```powershell
git tag -a releases/v1.0.2 -m "Release 1.0.2"
git push origin releases/v1.0.2
```

tag 指向当前提交。只有将 tag 推送到 GitHub 才会触发自动发布；只创建本地 tag 或只推送分支不会触发 `Release` 工作流。

### 查看结果

打开仓库的 **Actions → Release**，查看该 tag 对应的运行记录。全部成功后，在仓库 **Releases** 页面查看正式版本和下载附件。

## 4. tag 规则

触发条件使用通配符 `releases/v*`，随后由脚本严格校验正式版本格式。

| 示例 | 结果 |
| --- | --- |
| `releases/v1.0.2` | 触发；项目版本一致时继续构建 |
| `releases/v2.0.0` | 触发；项目版本一致时继续构建 |
| `releases/v1.0.2-rc.1` | 触发，但校验拒绝预发布版本 |
| `releases/v01.0.2` | 触发，但校验拒绝前导零 |
| `releases/v1.0` | 触发，但校验拒绝不完整版本 |
| `v1.0.2` | 不触发发布工作流 |

tag 不会自动修改项目版本号。例如项目仍为 `1.0.1` 时，推送 `releases/v1.0.2` 会在版本检查阶段失败。

## 5. 执行流程与产物

工作流按依赖顺序执行：

```text
推送正式 tag
  → version：核对 tag 与版本元数据
  → build：四个构建任务并行检查、打包并保存产物
  → publish：下载全部产物 → 创建/复用草稿 → 上传 → 公开正式 Release
```

每个构建任务执行 `npm ci`、前端单元测试、Rust 格式检查、Clippy 和 Rust 单元测试，再通过 Tauri 打包。Tauri 打包前执行 `npm run build`，完成 TypeScript 类型检查和前端构建。当前发布工作流未运行 Playwright E2E 测试。

Rust 构建使用 `--locked`，要求依赖与已提交的 `Cargo.lock` 一致。Rust 单元测试使用运行器临时目录作为应用数据目录。

| 平台 | 架构 | 发布附件 |
| --- | --- | --- |
| Windows | x64 | NSIS `.exe`、MSI `.msi` |
| Linux | x64 | `.deb`、`.rpm`、`.AppImage` |
| macOS | Apple Silicon / ARM64 | `.dmg` |
| macOS | Intel / x64 | `.dmg` |

构建产物先保存到 Actions 的 Artifacts，名称为 `release-<Rust target>`。公开 Release 的标题为 `PromLex vX.Y.Z`，说明由 GitHub 自动生成。

## 6. 失败处理与重试

| 失败位置 | Release 状态 | 处理方式 |
| --- | --- | --- |
| 版本检查 | 不创建新 Release | 检查 tag 和所有版本文件是否一致 |
| 检查、编译或打包 | 不创建新 Release | 查看失败任务的日志，定位代码、依赖或打包问题 |
| 下载产物、创建草稿 | 可能尚未创建 Release | 处理权限、产物或网络问题后重跑 |
| 上传或公开 Release | 通常保留草稿 | 重跑失败任务，补齐附件并完成发布 |
| 对已公开版本重跑发布任务 | 保持现有公开版本 | 工作流拒绝覆盖其附件；修改后发布新版本 |

临时网络或服务故障可以在对应运行页面使用 **Re-run failed jobs**。上传重试会复用同 tag 的草稿，并替换草稿中同名附件。

重跑使用原 tag 指向的代码。如果修复需要修改代码或版本文件，应提交修复并使用新的发布 tag；仅重跑旧任务不会包含新提交。

如果原构建产物已过期，仅重跑发布任务无法下载它们，需要重新运行构建任务或整个工作流。

## 7. 常见问题

**推送 tag 后没有运行记录**

检查 tag 是否已推送到正确仓库、名称是否匹配 `releases/v*`、tag 指向的提交是否包含工作流，以及 Actions 是否被禁用。工作流没有配置手动触发入口。

**提示 `Version mismatch`**

根据错误中的文件名核对版本。使用版本脚本统一更新，再确认更新后的文件已提交；tag 必须指向这些版本文件所在的提交。

**创建 Release 或上传时出现权限错误**

检查仓库或组织策略是否限制 Actions、第三方 Action 或 `contents: write` 权限。工作流已在 `publish` 任务声明所需权限。

**看到 Artifacts，但没有公开 Release**

查看其他平台构建及 `publish` 任务。只有四个构建任务全部成功，发布任务才会启动；上传全部附件成功后才会公开草稿。

## 8. 当前范围

- 自动发布正式版本，不发布预发布版本。
- 发布桌面安装包，不部署公网 Web 服务，也不构建移动端安装包。
- Windows 签名、macOS 签名和公证尚未配置；需要另行接入证书与 Secrets。
- 上传 Release 安装包不等于实现应用内自动更新；当前工作流未接入 Tauri updater。
- 首次运行仍需在 GitHub 上验证各平台实际打包结果。

## 9. 参考资料

- [GitHub Actions 工作流语法](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)
- [Tauri GitHub 自动发布指南](https://v2.tauri.app/distribute/pipelines/github/)
- [tauri-action 使用说明](https://github.com/tauri-apps/tauri-action)
