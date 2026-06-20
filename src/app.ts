/**
 * 批量重命名工具 — 前端主逻辑
 *
 * 采用 Vanilla TS + 直接 DOM 操作，零框架依赖。
 * 状态集中管理、UI 通过 updateUI() 全量刷新。
 *
 * 布局结构（左右分栏）：
 * ┌─────────────────────────────────────────────────┐
 * │ Header (应用名 + 状态指示器 + Spinner)           │
 * ├──────────────┬──────────────────────────────────┤
 * │ Sidebar      │ Preview Panel                    │
 * │ · 路径选择    │ · 文件计数                        │
 * │ · 模式切换    │ · 进度条                          │
 * │ · 规则参数    │ · 预览表格（复选框 + 状态标签）     │
 * │ · 过滤/排序   │ · 日志面板                        │
 * │ · 操作按钮    │                                  │
 * └──────────────┴──────────────────────────────────┘
 */

import {
  selectFolder,
  getPreview,
  executeRename,
  cancelOperation,
  onProgress,
} from "./api";
import type {
  PreviewItem,
  ExecuteResult,
  LogEntry,
  ModeType,
  SortBy,
  ProgressPayload,
} from "./types";

// ─── 全局状态（单一数据源，updateUI 从 state 读取并渲染）────

const state = {
  // 路径与递归
  rootPath: "",
  recursive: false,

  // 当前模式
  mode: "find_replace" as ModeType,

  // 模式A 参数：查找替换
  find: "",
  replace: "",
  useRegex: false,

  // 模式B 参数：序号命名
  prefix: "file",
  separator: "_",
  startNum: 1,

  // 模式C 参数：统一后缀
  targetSuffix: "jpg",
  forceAll: true,
  lowercase: true,

  // 过滤与排序
  filterExts: "",
  sortBy: "name" as SortBy,

  // 预览与勾选
  preview: [] as PreviewItem[],
  checked: new Set<number>(),

  // 异步状态
  loading: false,
  executing: false,

  // "执行完成"提示（持续 2s）
  completed: false,
  completedTimer: null as ReturnType<typeof setTimeout> | null,

  // 执行结果与进度
  results: [] as ExecuteResult[],
  progress: null as ProgressPayload | null,

  // 操作日志
  logs: [] as LogEntry[],
};

/** 是否处于忙状态（加载中或执行中） */
function busy(): boolean {
  return state.loading || state.executing;
}

// ─── 轻量 DOM 构建工具 ────────────────────────────────────

type AttrVal = string | number | boolean | EventListener;
type Attrs = Record<string, AttrVal>;

/**
 * 创建 HTML 元素并设置属性/子节点
 * 支持 className、事件绑定（onXxx）、style、布尔属性
 */
function h(
  tag: string,
  attrs?: Attrs,
  ...children: (string | Node)[]
): HTMLElement {
  const el = document.createElement(tag);
  if (attrs) {
    for (const [k, v] of Object.entries(attrs)) {
      if (k === "className") { el.className = String(v); }
      else if (k === "innerHTML") { el.innerHTML = String(v); }
      else if (k.startsWith("on")) { el.addEventListener(k.slice(2).toLowerCase(), v as EventListener); }
      else if (k === "style" && typeof v === "string") { el.setAttribute("style", v); }
      else if (typeof v === "boolean") { (el as any)[k] = v; }
      else if (k === "value" && tag === "input") { (el as HTMLInputElement).value = String(v); }
      else { el.setAttribute(k, String(v)); }
    }
  }
  for (const c of children) {
    if (typeof c === "string") el.appendChild(document.createTextNode(c));
    else if (c) el.appendChild(c);
  }
  return el;
}

/**
 * 创建文本输入框，绑定 onChange 回调
 * 每次输入触发 updateUI() 以同步按钮状态
 */
function textInput(
  value: string,
  placeholder: string,
  onChange: (v: string) => void,
  extra?: Attrs,
): HTMLInputElement {
  const inp = document.createElement("input");
  inp.type = "text";
  inp.value = value;
  inp.placeholder = placeholder;
  inp.addEventListener("input", () => onChange(inp.value));
  if (extra) Object.entries(extra).forEach(([k, v]) => inp.setAttribute(k, String(v)));
  return inp;
}

// ─── DOM 构建（一次性渲染，后续通过 updateUI 增量更新）─────────

/** 构建完整的应用 DOM 树，仅调用一次 */
function buildApp(root: HTMLElement): void {
  root.innerHTML = "";

  // ── 顶部标题栏：应用名 + 状态指示器 ──
  const header = h("div", { id: "header" },
    h("h1", {}, "批量重命名工具"),
    h("div", { className: "status", id: "status-area" },
      h("span", { className: "spinner", id: "spinner" }),
      h("span", { id: "status-text" }, "就绪"),
    ),
  );

  // ── 左侧配置面板 ──
  const sidebar = h("div", { id: "sidebar" },
    // 路径选择：文本框 + 浏览按钮 + 包含子目录复选框
    h("div", { className: "sc" },
      h("label", { className: "sc-label" }, "目标文件夹"),
      h("div", { className: "inline-row" },
        textInput(state.rootPath, "选择或输入路径...", (v) => { state.rootPath = v; updateUI(); }),
        h("button", { id: "btn-browse", onClick: onBrowse }, "浏览"),
      ),
      h("label", { className: "cb", id: "recursive-wrap" },
        h("input", { type: "checkbox", id: "recursive", onClick: () => { state.recursive = !state.recursive; } }),
        "包含子目录",
      ),
    ),

    // 模式切换 Tab（三个按钮平铺）
    h("div", { className: "sc" },
      h("label", { className: "sc-label" }, "重命名模式"),
      h("div", { className: "mode-tabs", id: "mode-tabs" },
        buildModeTab("查找替换", "find_replace"),
        buildModeTab("序号命名", "sequential"),
        buildModeTab("统一后缀", "suffix"),
      ),
    ),

    // 模式参数区域（根据当前模式动态渲染）
    h("div", { id: "mode-params" }),

    // 扩展名过滤
    h("div", { className: "sc" },
      h("label", { className: "sc-label" }, "扩展名过滤"),
      textInput(state.filterExts, "png,jpg,mp4 (留空=全部)", (v) => { state.filterExts = v; }),
    ),

    // 排序依据下拉框
    h("div", { className: "sc" },
      h("label", { className: "sc-label" }, "排序依据"),
      buildSortSelect(),
    ),

    // 操作按钮区
    h("div", { className: "sc" },
      h("div", { className: "inline-row", style: "margin-bottom:8px" },
        h("button", { className: "btn-primary btn-block", id: "btn-preview", onClick: onPreview }, "预览变更"),
      ),
      h("div", { className: "inline-row" },
        h("button", { className: "btn-danger btn-block", id: "btn-execute", onClick: onExecute }, "执行重命名"),
        h("button", { id: "btn-cancel", onClick: onCancel }, "取消"),
      ),
      h("div", { className: "inline-row", style: "margin-top:8px" },
        h("button", { onClick: onClear }, "清空"),
        h("button", { onClick: onToggleAll }, "全选/取消全选"),
      ),
      // 文件计数提示（动态更新）
      h("div", { style: "margin-top:8px;font-size:12px;color:var(--text2)", id: "file-count" }, "选择文件夹后点击预览"),
    ),
  );

  // ── 右侧预览面板 ──
  const preview = h("div", { id: "preview" },
    // 文件计数
    h("div", { id: "preview-header" },
      h("span", { className: "count", id: "preview-count" }, "0 个文件 / 0 个已选"),
    ),
    // 进度条
    h("div", { id: "progress-bar" },
      h("div", { id: "progress-fill" }),
    ),
    // 预览表格容器
    h("div", { id: "table-wrap" },
      h("div", { className: "empty" }, "点击左侧「预览变更」查看文件列表"),
    ),
    // 底部操作日志
    h("div", { id: "log-area" }),
  );

  const main = h("div", { id: "main" }, sidebar, preview);
  root.append(header, main);

  // 初始渲染模式参数区域
  renderModeParams();
}

/** 构建单个模式 Tab 按钮 */
function buildModeTab(label: string, mode: ModeType): HTMLElement {
  const cls = state.mode === mode ? "mode-tab active" : "mode-tab";
  return h("div", { className: cls, onClick: () => switchMode(mode) }, label);
}

/** 构建排序依据下拉框 */
function buildSortSelect(): HTMLElement {
  const sel = document.createElement("select");
  sel.id = "sort-select";
  const opt1 = document.createElement("option");
  opt1.value = "name";
  opt1.textContent = "文件名";
  if (state.sortBy === "name") opt1.selected = true;
  const opt2 = document.createElement("option");
  opt2.value = "modified";
  opt2.textContent = "修改时间";
  if (state.sortBy === "modified") opt2.selected = true;
  sel.append(opt1, opt2);
  sel.addEventListener("change", () => {
    state.sortBy = sel.value as SortBy;
  });
  return sel;
}

// ─── 模式切换与参数渲染 ────────────────────────────────────

/** 切换模式时重置对应参数 */
function resetModeParams(): void {
  state.find = "";
  state.replace = "";
  state.useRegex = false;
  state.prefix = "file";
  state.separator = "_";
  state.startNum = 1;
  state.targetSuffix = "jpg";
  state.forceAll = true;
  state.lowercase = true;
}

/** 切换重命名模式：更新 state + Tab 高亮 + 渲染参数区域 */
function switchMode(mode: ModeType): void {
  state.mode = mode;
  resetModeParams();
  document.querySelectorAll(".mode-tab").forEach((t) => {
    const el = t as HTMLElement;
    const tabMode = el.textContent === "查找替换" ? "find_replace"
      : el.textContent === "序号命名" ? "sequential" : "suffix";
    el.classList.toggle("active", tabMode === mode);
  });
  renderModeParams();
}

/** 根据当前 mode 渲染对应的参数输入区域 */
function renderModeParams(): void {
  const container = document.getElementById("mode-params");
  if (!container) return;
  container.innerHTML = "";

  if (state.mode === "find_replace") {
    // 模式A：查找文本 + 替换文本 + 正则复选框
    container.append(
      h("div", { className: "inline-row", style: "margin-bottom:6px" },
        h("div", {},
          h("label", { style: "font-size:11px;color:var(--text2);margin-bottom:3px" }, "查找"),
          textInput(state.find, "查找文本或正则", (v) => { state.find = v; }),
        ),
        h("div", {},
          h("label", { style: "font-size:11px;color:var(--text2);margin-bottom:3px" }, "替换"),
          textInput(state.replace, "替换文本", (v) => { state.replace = v; }),
        ),
      ),
      h("label", { className: "cb" },
        h("input", {
          type: "checkbox",
          checked: state.useRegex,
          onClick: () => { state.useRegex = !state.useRegex; },
        }),
        "使用正则表达式",
      ),
    );
  } else if (state.mode === "sequential") {
    // 模式B：前缀 + 连接符 + 起始编号
    container.append(
      h("div", { className: "inline-row" },
        h("div", {},
          h("label", { style: "font-size:11px;color:var(--text2);margin-bottom:3px" }, "前缀"),
          textInput(state.prefix, "prefix", (v) => { state.prefix = v; }),
        ),
        h("div", { className: "narrow" },
          h("label", { style: "font-size:11px;color:var(--text2);margin-bottom:3px" }, "连接符"),
          textInput(state.separator, "_", (v) => { state.separator = v; }),
        ),
        h("div", { className: "narrow" },
          h("label", { style: "font-size:11px;color:var(--text2);margin-bottom:3px" }, "起始"),
          h("input", {
            type: "number",
            value: String(state.startNum),
            placeholder: "1",
            onChange: (e: Event) => { state.startNum = parseInt((e.target as HTMLInputElement).value) || 1; },
          }),
        ),
      ),
      h("div", { style: "font-size:11px;color:var(--text2);margin-top:4px" }, "各组独立编号，位数自动补零"),
    );
  } else {
    // 模式C：目标后缀 + 强制转换复选框 + 转小写复选框
    container.append(
      h("div", { style: "margin-bottom:6px" },
        h("label", { style: "font-size:11px;color:var(--text2);margin-bottom:3px" }, "目标后缀（不带点号）"),
        textInput(state.targetSuffix, "jpg", (v) => { state.targetSuffix = v; }),
      ),
      h("label", { className: "cb" },
        h("input", {
          type: "checkbox",
          checked: state.forceAll,
          onClick: () => { state.forceAll = !state.forceAll; },
        }),
        "强制转换所有扩展名",
      ),
      h("label", { className: "cb" },
        h("input", {
          type: "checkbox",
          checked: state.lowercase,
          onClick: () => { state.lowercase = !state.lowercase; },
        }),
        "将后缀转为小写",
      ),
    );
  }
}

// ─── 用户操作处理 ──────────────────────────────────────────

/** 浏览按钮：弹出系统文件夹选择对话框 */
async function onBrowse(): Promise<void> {
  if (busy()) return;
  const folder = await selectFolder();
  if (folder) {
    state.rootPath = folder;
    // 同步更新文本框显示的路径
    const inp = document.querySelector("#sidebar input[type='text']") as HTMLInputElement;
    if (inp) inp.value = folder;
    updateUI();
  }
}

/** 预览按钮：调用后端 get_preview，展示新旧文件名映射 */
async function onPreview(): Promise<void> {
  if (!state.rootPath || busy()) return;
  state.loading = true;
  // 清空上一轮数据
  state.preview = [];
  state.checked.clear();
  state.results = [];
  state.logs = [];
  updateUI();

  try {
    const items = await getPreview(
      state.rootPath,
      state.recursive,
      buildModeObj(),
      state.filterExts || null,
      state.sortBy,
    );
    state.preview = items;
    // 默认勾选所有状态为 ok 的文件
    state.checked = new Set(items.filter((it) => it.status === "ok").map((it) => it.id));
    addLog(true, `预览完成，共 ${items.length} 个文件`);
  } catch (e) {
    addLog(false, `预览失败: ${String(e)}`);
  } finally {
    state.loading = false;
    updateUI();
  }
}

/** 执行按钮：调用后端 execute_rename，批量重命名 */
async function onExecute(): Promise<void> {
  if (!state.rootPath || busy()) return;
  // 收集用户勾选且状态为 ok 的文件 ID
  const ids = state.preview
    .filter((it) => state.checked.has(it.id) && it.status === "ok")
    .map((it) => it.id);
  if (ids.length === 0) {
    addLog(false, "没有可执行的文件");
    return;
  }

  state.executing = true;
  state.results = [];
  state.progress = null;
  updateUI();

  try {
    const results = await executeRename(
      state.rootPath,
      state.recursive,
      buildModeObj(),
      state.filterExts || null,
      state.sortBy,
      ids,
    );
    state.results = results;
    state.progress = null;
    state.executing = false;
    const okCount = results.filter((r) => r.success).length;
    addLog(true, `完成: ${okCount} 成功, ${results.length - okCount} 失败`);

    // 显示"执行完成"状态，2 秒后自动恢复
    state.completed = true;
    if (state.completedTimer) clearTimeout(state.completedTimer);
    state.completedTimer = setTimeout(() => {
      state.completed = false;
      state.completedTimer = null;
      updateUI();
    }, 2000);
    updateUI();

    // 执行完成后自动刷新预览
    await onPreview();
  } catch (e) {
    addLog(false, `执行失败: ${String(e)}`);
    state.executing = false;
    state.completed = false;
    if (state.completedTimer) { clearTimeout(state.completedTimer); state.completedTimer = null; }
    updateUI();
  }
}

/** 取消按钮：调用后端 cancel_operation 中断操作 */
async function onCancel(): Promise<void> {
  if (!state.executing) return;
  await cancelOperation();
  state.executing = false;
  state.completed = false;
  if (state.completedTimer) { clearTimeout(state.completedTimer); state.completedTimer = null; }
  addLog(false, "操作已取消");
  updateUI();
}

/** 清空按钮：重置预览、勾选、结果、日志 */
function onClear(): void {
  state.preview = [];
  state.checked.clear();
  state.results = [];
  state.logs = [];
  state.progress = null;
  state.executing = false;
  state.completed = false;
  if (state.completedTimer) { clearTimeout(state.completedTimer); state.completedTimer = null; }
  updateUI();
}

/** 全选/取消全选：仅操作状态为 ok 的文件 */
function onToggleAll(): void {
  if (state.checked.size > 0) {
    state.checked.clear();
  } else {
    state.preview.forEach((it) => {
      if (it.status === "ok") state.checked.add(it.id);
    });
  }
  updateUI();
}

// ─── 辅助函数 ──────────────────────────────────────────────

/** 根据当前 state 构建发送给后端的 RenameMode 对象 */
function buildModeObj() {
  if (state.mode === "find_replace") {
    return {
      type: "find_replace" as const,
      params: { find: state.find, replace: state.replace, use_regex: state.useRegex },
    };
  } else if (state.mode === "sequential") {
    return {
      type: "sequential" as const,
      params: { prefix: state.prefix, separator: state.separator, start: state.startNum },
    };
  } else {
    return {
      type: "suffix" as const,
      params: {
        target_suffix: state.targetSuffix,
        force_all: state.forceAll,
        lowercase: state.lowercase,
      },
    };
  }
}

/** 添加一条日志到面板（自动带时间戳） */
function addLog(ok: boolean, text: string): void {
  const now = new Date();
  const time = now.toTimeString().slice(0, 8); // HH:MM:SS
  state.logs.push({ time, text, ok });
  renderLogs();
}

/** 将日志数组渲染到页面底部日志区域 */
function renderLogs(): void {
  const area = document.getElementById("log-area");
  if (!area) return;
  area.innerHTML = state.logs
    .map(
      (e) => `<div class="log-entry ${e.ok ? "log-ok" : "log-err"}"><span class="log-time">${e.time}</span>${e.text}</div>`,
    )
    .join("");
  area.scrollTop = area.scrollHeight; // 自动滚到底部
}

// ─── UI 刷新（每次 state 变更后调用）────────────────────────

/** 全局 UI 刷新函数：根据当前 state 更新所有 DOM 状态 */
function updateUI(): void {
  // ── 顶部状态指示器 ──
  // 优先级：completed > executing > loading > 就绪
  const spinner = document.getElementById("spinner")!;
  const statusText = document.getElementById("status-text")!;
  if (state.completed) {
    spinner.classList.remove("active");
    statusText.textContent = "执行完成";
  } else if (state.executing) {
    spinner.classList.add("active");
    statusText.textContent = `执行中${state.progress ? ` (${state.progress.current}/${state.progress.total})` : ""}`;
  } else if (state.loading) {
    spinner.classList.add("active");
    statusText.textContent = "加载中...";
  } else {
    spinner.classList.remove("active");
    statusText.textContent = "就绪";
  }

  // ── 进度条 ──
  // 有具体进度时精确显示，否则忙状态时显示满条动画
  const fill = document.getElementById("progress-fill")!;
  if (state.progress) {
    fill.style.width = `${(state.progress.current / state.progress.total) * 100}%`;
  } else {
    fill.style.width = state.loading || state.executing ? "100%" : "0%";
  }

  // ── 按钮启用/禁用状态 ──
  const btnPreview = document.getElementById("btn-preview") as HTMLButtonElement;
  const btnExecute = document.getElementById("btn-execute") as HTMLButtonElement;
  const btnCancel = document.getElementById("btn-cancel") as HTMLButtonElement;
  const btnBrowse = document.getElementById("btn-browse") as HTMLButtonElement;
  if (btnPreview) btnPreview.disabled = busy() || !state.rootPath;
  if (btnExecute) btnExecute.disabled = busy() || state.checked.size === 0;
  if (btnCancel) btnCancel.disabled = !state.executing;
  if (btnBrowse) btnBrowse.disabled = busy();

  // ── 文件计数提示（左侧面板底部） ──
  const fc = document.getElementById("file-count");
  if (fc) {
    fc.textContent = state.preview.length > 0
      ? `${state.preview.length} 个文件, ${state.checked.size} 个已选`
      : "选择文件夹后点击预览";
  }

  // ── 预览表头计数 ──
  const pc = document.getElementById("preview-count");
  if (pc) {
    pc.textContent = `${state.preview.length} 个文件 / ${state.checked.size} 个已选`;
  }

  // ── 表格与日志 ──
  renderTable();
  renderLogs();
}

/** 渲染预览表格（全量重建 DOM，简单直接） */
function renderTable(): void {
  const wrap = document.getElementById("table-wrap");
  if (!wrap) return;

  // 空状态提示
  if (state.preview.length === 0 && !state.loading) {
    wrap.innerHTML = '<div class="empty">点击左侧「预览变更」查看文件列表</div>';
    return;
  }

  const cols = ["", "相对路径(25%)", "原文件名", "新文件名", "状态"];

  const table = document.createElement("table");
  const thead = document.createElement("thead");
  const headerRow = document.createElement("tr");
  // 创建表头列
  cols.forEach((label, i) => {
    const th = document.createElement("th");
    th.textContent = label;
    if (i === 0) th.style.width = "30px";
    else if (i === 1) th.style.width = "25%";
    else if (i === 4) th.style.width = "80px";
    headerRow.appendChild(th);
  });
  thead.appendChild(headerRow);
  table.appendChild(thead);

  const tbody = document.createElement("tbody");

  // 统计所有 ok 状态且已全选的文件
  const allOk = state.preview.filter((it) => it.status === "ok");
  const hasAllChecked = allOk.length > 0 && allOk.every((it) => state.checked.has(it.id));

  // ── 表头全选复选框 ──
  const thCheck = headerRow.children[0] as HTMLElement;
  const hcb = document.createElement("input");
  hcb.type = "checkbox";
  hcb.checked = hasAllChecked;
  hcb.addEventListener("change", () => {
    if (hcb.checked) {
      allOk.forEach((it) => state.checked.add(it.id));
    } else {
      state.checked.clear();
    }
    updateUI();
  });
  thCheck.appendChild(hcb);

  // ── 逐行渲染 ──
  for (const item of state.preview) {
    const tr = document.createElement("tr");
    // 未勾选行降低透明度
    if (!state.checked.has(item.id)) tr.classList.add("row-disabled");

    // 复选框列
    const td0 = document.createElement("td");
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = state.checked.has(item.id);
    cb.addEventListener("change", () => {
      if (cb.checked) state.checked.add(item.id);
      else state.checked.delete(item.id);
      updateUI();
    });
    td0.appendChild(cb);
    tr.appendChild(td0);

    // 相对路径列
    const td1 = document.createElement("td");
    td1.textContent = item.relative_path || "./";
    td1.title = item.relative_path;
    tr.appendChild(td1);

    // 原文件名列
    const td2 = document.createElement("td");
    td2.textContent = item.old_name;
    td2.title = item.old_name;
    tr.appendChild(td2);

    // 新文件名列
    const td3 = document.createElement("td");
    td3.textContent = item.new_name;
    td3.title = item.new_name;
    tr.appendChild(td3);

    // 状态标签列（带颜色标识）
    const td4 = document.createElement("td");
    const badge = document.createElement("span");
    badge.className = `badge badge-${item.status}`;
    badge.textContent = item.status === "ok" ? "待执行" : item.status === "conflict" ? "冲突" : "无变化";
    td4.appendChild(badge);
    tr.appendChild(td4);

    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  wrap.innerHTML = "";
  wrap.appendChild(table);
}

// ─── 初始化入口 ────────────────────────────────────────────

/** 应用启动：构建 DOM → 注册进度事件监听 → 首次渲染 */
export function init(): void {
  const root = document.getElementById("app")!;
  buildApp(root);

  // 监听后端 rename-progress 事件，实时更新进度条
  onProgress((p) => {
    state.progress = p;
    updateUI();
  });

  updateUI();
}
