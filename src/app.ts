/**
 * 批量重命名工具 — 前端主逻辑（v2）
 *
 * Vanilla TypeScript + 原生 DOM，无框架、无第三方依赖。
 *
 * 设计原则：
 * - 一次性构建静态骨架，动态区域（模式参数面板 / 表格 / 日志 / toast）局部更新；
 * - 单一状态源 `state`，所有交互写入 state 后再同步 UI；
 * - 用户数据（路径、文件名、日志）一律通过 textContent / 文本节点写入，绝不拼 innerHTML；
 * - 无 Tauri 环境的普通浏览器中可正常加载，仅后端操作会失败并被 toast 捕获。
 *
 * 布局：
 * ┌──────────────────────────────────────────────────────────┐
 * │ Header 56px：logo + 标题/副标题 + 状态 pill + 主题切换      │
 * ├───────────────┬──────────────────────────────────────────┤
 * │ Sidebar 360px │ Main                                     │
 * │ · 目标文件夹   │ · 进度条（2px）                           │
 * │ · 4 模式 tab  │ · 统计 chips + 搜索 + 全选                │
 * │ · 模式参数    │ · 预览表格（sticky 表头）                 │
 * │ · 过滤/排序   │ · 日志面板 110px                          │
 * │ · 操作区(固定)│ · toast 浮层                              │
 * └───────────────┴──────────────────────────────────────────┘
 */

import {
  cancelOperation,
  executeRename,
  getPreview,
  getUndoStatus,
  onProgress,
  selectFolder,
  undoLastRename,
} from "./api";
import type {
  ExecuteResult,
  FindReplaceParams,
  LogEntry,
  LogKind,
  ModeType,
  NumberScope,
  PatternParams,
  PreviewItem,
  ProgressPayload,
  RenameMode,
  SequentialParams,
  SortBy,
  Status,
  SuffixParams,
  Theme,
  UndoStatus,
} from "./types";

// ─── 常量 ──────────────────────────────────────────────────

const SETTINGS_KEY = "rename-rust:settings:v1";
const TOAST_DURATION = 3000;
const COMPLETED_DURATION = 2000;
const MAX_LOGS = 300;
const MAX_TOASTS = 4;

const MODE_TYPES: readonly ModeType[] = [
  "find_replace",
  "sequential",
  "pattern",
  "suffix",
];

const MODE_LABELS: Record<ModeType, string> = {
  find_replace: "查找替换",
  sequential: "序号命名",
  pattern: "模板命名",
  suffix: "统一后缀",
};

const MODE_ICONS: Record<ModeType, IconName> = {
  find_replace: "replace",
  sequential: "hash",
  pattern: "braces",
  suffix: "tag",
};

const STATUS_LABELS: Record<Status, string> = {
  ok: "待执行",
  conflict: "冲突",
  unchanged: "无变化",
};

// ─── 设置模型与持久化 ──────────────────────────────────────

interface PersistedSettings {
  mode: ModeType;
  findReplace: FindReplaceParams;
  sequential: SequentialParams;
  pattern: PatternParams;
  suffix: SuffixParams;
  filterExts: string;
  sortBy: SortBy;
  recursive: boolean;
}

type Activity = "idle" | "preview" | "execute" | "undo";

interface AppState extends PersistedSettings {
  theme: Theme;
  /** 主题是否来自用户手动切换（false 时继续跟随系统） */
  themeManual: boolean;

  rootPath: string;
  preview: PreviewItem[];
  checked: Set<number>;
  search: string;

  activity: Activity;
  progress: ProgressPayload | null;
  completed: boolean;
  completedTimer: ReturnType<typeof setTimeout> | null;

  logs: LogEntry[];
  undo: UndoStatus;
  logCollapsed: boolean;
}

function freshDefaults(): PersistedSettings {
  return {
    mode: "find_replace",
    findReplace: {
      find: "",
      replace: "",
      use_regex: false,
      case_sensitive: true,
      whole_word: false,
      first_only: false,
      include_extension: false,
    },
    sequential: {
      prefix: "file",
      separator: "_",
      start: 1,
      step: 1,
      pad_width: 0,
      suffix: "",
      scope: "per_dir",
    },
    pattern: {
      template: "{name}_{index}",
      start: 1,
      step: 1,
      pad_width: 0,
      scope: "per_dir",
    },
    suffix: {
      target_suffix: "jpg",
      force_all: true,
      lowercase: true,
    },
    filterExts: "",
    sortBy: "name",
    recursive: false,
  };
}

type UnknownRecord = Record<string, unknown>;

function isRecord(value: unknown): value is UnknownRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function readString(
  source: UnknownRecord,
  key: string,
  fallback: string,
): string {
  const value = source[key];
  return typeof value === "string" ? value : fallback;
}

function readBoolean(
  source: UnknownRecord,
  key: string,
  fallback: boolean,
): boolean {
  const value = source[key];
  return typeof value === "boolean" ? value : fallback;
}

function readInt(
  source: UnknownRecord,
  key: string,
  fallback: number,
  min: number,
  max: number,
): number {
  const value = source[key];
  if (typeof value !== "number" || !Number.isFinite(value)) return fallback;
  return Math.min(max, Math.max(min, Math.round(value)));
}

function readEnum<T extends string>(
  source: UnknownRecord,
  key: string,
  allowed: readonly T[],
  fallback: T,
): T {
  const value = source[key];
  return typeof value === "string" && (allowed as readonly string[]).includes(value)
    ? (value as T)
    : fallback;
}

function loadSettings(): { settings: PersistedSettings; theme?: Theme } {
  const settings = freshDefaults();
  let theme: Theme | undefined;
  try {
    const raw = localStorage.getItem(SETTINGS_KEY);
    if (!raw) return { settings };
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed)) return { settings };

    if (parsed.theme === "dark" || parsed.theme === "light") {
      theme = parsed.theme;
    }
    settings.mode = readEnum<ModeType>(
      parsed,
      "mode",
      MODE_TYPES,
      settings.mode,
    );
    settings.recursive = readBoolean(
      parsed,
      "recursive",
      settings.recursive,
    );
    settings.filterExts = readString(
      parsed,
      "filterExts",
      settings.filterExts,
    );
    settings.sortBy = readEnum<SortBy>(
      parsed,
      "sortBy",
      ["name", "modified", "created"],
      settings.sortBy,
    );

    const fr = parsed.findReplace;
    if (isRecord(fr)) {
      settings.findReplace = {
        find: readString(fr, "find", settings.findReplace.find),
        replace: readString(fr, "replace", settings.findReplace.replace),
        use_regex: readBoolean(fr, "use_regex", settings.findReplace.use_regex),
        case_sensitive: readBoolean(
          fr,
          "case_sensitive",
          settings.findReplace.case_sensitive,
        ),
        whole_word: readBoolean(fr, "whole_word", settings.findReplace.whole_word),
        first_only: readBoolean(fr, "first_only", settings.findReplace.first_only),
        include_extension: readBoolean(
          fr,
          "include_extension",
          settings.findReplace.include_extension,
        ),
      };
    }

    const seq = parsed.sequential;
    if (isRecord(seq)) {
      settings.sequential = {
        prefix: readString(seq, "prefix", settings.sequential.prefix),
        separator: readString(seq, "separator", settings.sequential.separator),
        start: readInt(seq, "start", settings.sequential.start, 0, 1_000_000_000),
        step: readInt(seq, "step", settings.sequential.step, 1, 1_000_000_000),
        pad_width: readInt(seq, "pad_width", settings.sequential.pad_width, 0, 20),
        suffix: readString(seq, "suffix", settings.sequential.suffix),
        scope: readEnum<NumberScope>(
          seq,
          "scope",
          ["per_dir", "global"],
          settings.sequential.scope,
        ),
      };
    }

    const pat = parsed.pattern;
    if (isRecord(pat)) {
      settings.pattern = {
        template: readString(pat, "template", settings.pattern.template),
        start: readInt(pat, "start", settings.pattern.start, 0, 1_000_000_000),
        step: readInt(pat, "step", settings.pattern.step, 1, 1_000_000_000),
        pad_width: readInt(pat, "pad_width", settings.pattern.pad_width, 0, 20),
        scope: readEnum<NumberScope>(
          pat,
          "scope",
          ["per_dir", "global"],
          settings.pattern.scope,
        ),
      };
    }

    const suf = parsed.suffix;
    if (isRecord(suf)) {
      settings.suffix = {
        target_suffix: readString(
          suf,
          "target_suffix",
          settings.suffix.target_suffix,
        ),
        force_all: readBoolean(suf, "force_all", settings.suffix.force_all),
        lowercase: readBoolean(suf, "lowercase", settings.suffix.lowercase),
      };
    }
  } catch {
    // localStorage 不可用或 JSON 损坏：回退默认值
  }
  return { settings, theme };
}

function systemTheme(): Theme {
  if (
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-color-scheme: light)").matches
  ) {
    return "light";
  }
  return "dark";
}

function createInitialState(): AppState {
  const loaded = loadSettings();
  return {
    ...loaded.settings,
    theme: loaded.theme ?? systemTheme(),
    themeManual: loaded.theme !== undefined,
    rootPath: "",
    preview: [],
    checked: new Set<number>(),
    search: "",
    activity: "idle",
    progress: null,
    completed: false,
    completedTimer: null,
    logs: [],
    undo: { available: false, count: 0 },
    logCollapsed: false,
  };
}

function persist(): void {
  try {
    const payload = {
      theme: state.theme,
      mode: state.mode,
      findReplace: state.findReplace,
      sequential: state.sequential,
      pattern: state.pattern,
      suffix: state.suffix,
      filterExts: state.filterExts,
      sortBy: state.sortBy,
      recursive: state.recursive,
    };
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(payload));
  } catch {
    // 隐私模式 / 配额不足时静默失败，不影响主流程
  }
}

// ─── 轻量 DOM / SVG 构建工具 ──────────────────────────────

type Child = Node | string | null | undefined | false;

interface ElOptions {
  class?: string;
  id?: string;
  text?: string;
  title?: string;
  attrs?: Record<string, string | number | boolean | null | undefined>;
  props?: Record<string, unknown>;
  on?: Partial<Record<keyof HTMLElementEventMap, EventListener>>;
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  options: ElOptions = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (options.class) node.className = options.class;
  if (options.id) node.id = options.id;
  if (options.text !== undefined) node.textContent = options.text;
  if (options.title !== undefined) node.title = options.title;
  if (options.attrs) {
    for (const [name, value] of Object.entries(options.attrs)) {
      if (value === undefined || value === null || value === false) continue;
      node.setAttribute(name, value === true ? "" : String(value));
    }
  }
  if (options.props) Object.assign(node, options.props);
  if (options.on) {
    for (const [name, handler] of Object.entries(options.on)) {
      if (handler) node.addEventListener(name, handler);
    }
  }
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    node.append(typeof child === "string" ? document.createTextNode(child) : child);
  }
  return node;
}

interface IconPart {
  tag: "path" | "circle" | "rect" | "line";
  attrs: Record<string, string>;
}

const ICONS = {
  pen: [
    { tag: "path", attrs: { d: "M4 20l4-1 10.5-10.5a2.12 2.12 0 0 0-3-3L5 16l-1 4z" } },
    { tag: "path", attrs: { d: "M13.5 6.5l3 3" } },
  ],
  folder: [
    {
      tag: "path",
      attrs: {
        d: "M3 7a2 2 0 0 1 2-2h4l2 2h6a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
      },
    },
  ],
  "folder-open": [
    {
      tag: "path",
      attrs: {
        d: "M3 7a2 2 0 0 1 2-2h4l2 2h6a2 2 0 0 1 2 2v2",
      },
    },
    { tag: "path", attrs: { d: "M3 7v10a2 2 0 0 0 2 2h13l3-8H6l-3 8" } },
  ],
  search: [
    { tag: "circle", attrs: { cx: "11", cy: "11", r: "7" } },
    { tag: "path", attrs: { d: "M20 20l-3.5-3.5" } },
  ],
  sun: [
    { tag: "circle", attrs: { cx: "12", cy: "12", r: "4" } },
    { tag: "path", attrs: { d: "M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" } },
  ],
  moon: [
    {
      tag: "path",
      attrs: { d: "M20.5 13.2A8.5 8.5 0 1 1 10.8 3.5a6.6 6.6 0 0 0 9.7 9.7z" },
    },
  ],
  replace: [
    { tag: "path", attrs: { d: "M4 7h11" } },
    { tag: "path", attrs: { d: "M12 4l3 3-3 3" } },
    { tag: "path", attrs: { d: "M20 17H9" } },
    { tag: "path", attrs: { d: "M12 14l-3 3 3 3" } },
  ],
  hash: [
    { tag: "path", attrs: { d: "M4 9h16M4 15h16M10 3 8 21M16 3l-2 18" } },
  ],
  braces: [
    {
      tag: "path",
      attrs: {
        d: "M8 3H7a2 2 0 0 0-2 2v4a2 2 0 0 1-2 2 2 2 0 0 1 2 2v4a2 2 0 0 0 2 2h1",
      },
    },
    {
      tag: "path",
      attrs: {
        d: "M16 3h1a2 2 0 0 1 2 2v4a2 2 0 0 0 2 2 2 2 0 0 0-2 2v4a2 2 0 0 1-2 2h-1",
      },
    },
  ],
  tag: [
    {
      tag: "path",
      attrs: {
        d: "M20.6 13.4 13.4 20.6a2 2 0 0 1-2.8 0l-7.2-7.2A2 2 0 0 1 3 12V5a2 2 0 0 1 2-2h7a2 2 0 0 1 1.4.6l7.2 7.2a2 2 0 0 1 0 2.6z",
      },
    },
    { tag: "circle", attrs: { cx: "7.5", cy: "7.5", r: "0.75" } },
  ],
  "chevron-down": [{ tag: "path", attrs: { d: "m6 9 6 6 6-6" } }],
  "chevron-up": [{ tag: "path", attrs: { d: "m6 15 6-6 6 6" } }],
  x: [
    { tag: "path", attrs: { d: "M18 6 6 18" } },
    { tag: "path", attrs: { d: "M6 6l12 12" } },
  ],
  "check-circle": [
    { tag: "circle", attrs: { cx: "12", cy: "12", r: "9" } },
    { tag: "path", attrs: { d: "m8.5 12 2.5 2.5 4.5-5" } },
  ],
  "alert-circle": [
    { tag: "circle", attrs: { cx: "12", cy: "12", r: "9" } },
    { tag: "path", attrs: { d: "M12 7.5v5" } },
    { tag: "circle", attrs: { cx: "12", cy: "16", r: "0.75" } },
  ],
  info: [
    { tag: "circle", attrs: { cx: "12", cy: "12", r: "9" } },
    { tag: "path", attrs: { d: "M12 11v5" } },
    { tag: "circle", attrs: { cx: "12", cy: "7.5", r: "0.75" } },
  ],
  undo: [
    { tag: "path", attrs: { d: "M3 7v6h6" } },
    { tag: "path", attrs: { d: "M3.6 13a9 9 0 1 0 2.6-7.4L3 8" } },
  ],
  trash: [
    { tag: "path", attrs: { d: "M4 7h16" } },
    { tag: "path", attrs: { d: "M10 11v6M14 11v6" } },
    {
      tag: "path",
      attrs: {
        d: "M6.5 7 7.5 20a2 2 0 0 0 2 1.8h5a2 2 0 0 0 2-1.8L17.5 7",
      },
    },
    { tag: "path", attrs: { d: "M9 7V5a2 2 0 0 1 2-2h2a2 2 0 0 1 2 2v2" } },
  ],
  play: [{ tag: "path", attrs: { d: "m8.5 5 10.5 7-10.5 7z" } }],
  terminal: [
    { tag: "path", attrs: { d: "M4 17l5-5-5-5" } },
    { tag: "path", attrs: { d: "M12 19h8" } },
  ],
  file: [
    {
      tag: "path",
      attrs: { d: "M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" },
    },
    { tag: "path", attrs: { d: "M14 3v5h5" } },
  ],
  check: [{ tag: "path", attrs: { d: "m5 12 5 5L20 6" } }],
} satisfies Record<string, readonly IconPart[]>;

type IconName = keyof typeof ICONS;

function icon(name: IconName, size = 16, className?: string): SVGSVGElement {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("width", String(size));
  svg.setAttribute("height", String(size));
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "1.5");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("stroke-linejoin", "round");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");
  if (className) svg.setAttribute("class", className);
  for (const part of ICONS[name]) {
    const node = document.createElementNS(ns, part.tag);
    for (const [key, value] of Object.entries(part.attrs)) {
      node.setAttribute(key, value);
    }
    svg.append(node);
  }
  return svg;
}

// ─── 表单组件 ─────────────────────────────────────────────

interface TextFieldOptions {
  label: string;
  value: string;
  placeholder?: string;
  ariaLabel?: string;
  mono?: boolean;
  onInput: (value: string) => void;
}

function buildTextField(options: TextFieldOptions): HTMLElement {
  const input = el("input", {
    class: options.mono ? "input mono" : "input",
    attrs: {
      type: "text",
      placeholder: options.placeholder ?? "",
      "aria-label": options.ariaLabel ?? options.label,
      spellcheck: "false",
      autocomplete: "off",
    },
    props: { value: options.value },
  });
  input.addEventListener("input", () => options.onInput(input.value));
  return el(
    "label",
    { class: "field" },
    el("span", { class: "field-label", text: options.label }),
    input,
  );
}

interface NumberFieldOptions {
  label: string;
  value: number;
  min?: number;
  max?: number;
  onChange: (value: number) => void;
}

function clampInt(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  return Math.min(max, Math.max(min, Math.round(value)));
}

function buildNumberField(options: NumberFieldOptions): HTMLElement {
  const min = options.min ?? 0;
  const max = options.max ?? 1_000_000_000;
  let current = clampInt(options.value, min, max);
  const input = el("input", {
    class: "input mono",
    attrs: {
      type: "number",
      min: String(min),
      max: String(max),
      step: "1",
      inputmode: "numeric",
      "aria-label": options.label,
    },
    props: { value: String(current) },
  });
  input.addEventListener("input", () => {
    const parsed = Number.parseInt(input.value, 10);
    if (!Number.isFinite(parsed)) return;
    current = clampInt(parsed, min, max);
    options.onChange(current);
  });
  input.addEventListener("blur", () => {
    input.value = String(current);
  });
  return el(
    "label",
    { class: "field" },
    el("span", { class: "field-label", text: options.label }),
    input,
  );
}

interface SelectOption {
  value: string;
  label: string;
}

interface SelectFieldOptions {
  label: string;
  value: string;
  options: readonly SelectOption[];
  ariaLabel?: string;
  onChange: (value: string) => void;
}

function buildSelectField(options: SelectFieldOptions): HTMLElement {
  const select = el("select", {
    class: "select",
    attrs: { "aria-label": options.ariaLabel ?? options.label },
  });
  for (const option of options.options) {
    select.append(
      el("option", {
        attrs: { value: option.value },
        props: { selected: option.value === options.value },
        text: option.label,
      }),
    );
  }
  select.value = options.value;
  select.addEventListener("change", () => options.onChange(select.value));
  return el(
    "label",
    { class: "field" },
    el("span", { class: "field-label", text: options.label }),
    select,
  );
}

function buildCheckbox(
  label: string,
  checked: boolean,
  onChange: (checked: boolean) => void,
): HTMLElement {
  const input = el("input", {
    class: "check-input",
    attrs: { type: "checkbox" },
    props: { checked },
  });
  input.addEventListener("change", () => onChange(input.checked));
  return el(
    "label",
    { class: "check" },
    input,
    el("span", { class: "check-label", text: label }),
  );
}

interface SwitchOptions {
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}

function buildSwitch(options: SwitchOptions): {
  row: HTMLElement;
  input: HTMLInputElement;
} {
  const input = el("input", {
    class: "switch-input",
    attrs: { type: "checkbox" },
    props: { checked: options.checked },
  });
  input.addEventListener("change", () => options.onChange(input.checked));
  const row = el(
    "label",
    { class: "switch-row" },
    el("span", { class: "switch-label", text: options.label }),
    el(
      "span",
      { class: "switch-control" },
      input,
      el("span", { class: "switch-track" }, el("span", { class: "switch-thumb" })),
    ),
  );
  return { row, input };
}

function buildScopeControl(
  current: NumberScope,
  onChange: (scope: NumberScope) => void,
): HTMLElement {
  const buttons: HTMLButtonElement[] = [];
  const apply = (scope: NumberScope): void => {
    for (const button of buttons) {
      const active = button.dataset.scope === scope;
      button.classList.toggle("active", active);
      button.setAttribute("aria-pressed", String(active));
    }
  };
  const createButton = (scope: NumberScope, label: string): HTMLButtonElement => {
    const button = el("button", {
      class: "seg",
      attrs: { type: "button", "aria-pressed": String(scope === current) },
      text: label,
      on: {
        click: () => {
          onChange(scope);
          apply(scope);
        },
      },
    });
    button.dataset.scope = scope;
    buttons.push(button);
    return button;
  };
  const perDir = createButton("per_dir", "每个目录");
  const global = createButton("global", "全局连续");
  apply(current);
  return el(
    "div",
    { class: "seg-row" },
    el("span", { class: "field-label", text: "编号范围" }),
    el("div", { class: "segmented" }, perDir, global),
  );
}

function buildSection(title: string, ...children: Child[]): HTMLElement {
  return el(
    "section",
    { class: "section" },
    el("div", { class: "section-label", text: title }),
    ...children,
  );
}

// ─── UI 引用表 ────────────────────────────────────────────

interface RowRef {
  row: HTMLTableRowElement;
  checkbox: HTMLInputElement;
}

interface UiRefs {
  statusPill: HTMLElement;
  statusText: HTMLElement;
  spinner: HTMLElement;
  themeToggle: HTMLButtonElement;

  pathInput: HTMLInputElement;
  browseBtn: HTMLButtonElement;
  recursiveToggle: HTMLInputElement;

  modeTabs: Record<ModeType, HTMLButtonElement>;
  modePanels: Record<ModeType, HTMLElement>;

  previewBtn: HTMLButtonElement;
  executeBtn: HTMLButtonElement;
  cancelBtn: HTMLButtonElement;
  undoBtn: HTMLButtonElement;
  clearBtn: HTMLButtonElement;
  selectAllBtn: HTMLButtonElement;
  fileCount: HTMLElement;

  chipCounts: Record<Status, HTMLElement>;
  searchInput: HTMLInputElement;
  selectAllCheckbox: HTMLInputElement;
  headerCheckbox: HTMLInputElement;

  progress: HTMLElement;
  progressFill: HTMLElement;

  table: HTMLTableElement;
  tbody: HTMLTableSectionElement;
  tableScroll: HTMLElement;
  emptyState: HTMLElement;
  emptyIcon: HTMLElement;
  emptyTitle: HTMLElement;
  emptyHint: HTMLElement;

  logPanel: HTMLElement;
  logBody: HTMLElement;
  logCount: HTMLElement;
  logToggleBtn: HTMLButtonElement;

  toastContainer: HTMLElement;

  rowMap: Map<number, RowRef>;
}

// ─── 全局状态 / UI 引用 ───────────────────────────────────

let state: AppState;
let ui: UiRefs;

const toastTimers = new WeakMap<HTMLElement, number>();

// ─── 小工具 ──────────────────────────────────────────────

function selectedOkItems(): PreviewItem[] {
  return state.preview.filter(
    (item) => item.status === "ok" && state.checked.has(item.id),
  );
}

function countStatuses(items: readonly PreviewItem[]): Record<Status, number> {
  const result: Record<Status, number> = { ok: 0, conflict: 0, unchanged: 0 };
  for (const item of items) result[item.status] += 1;
  return result;
}

function visibleItems(): PreviewItem[] {
  const query = state.search.trim().toLowerCase();
  if (!query) return state.preview;
  return state.preview.filter(
    (item) =>
      item.relative_path.toLowerCase().includes(query) ||
      item.old_name.toLowerCase().includes(query) ||
      item.new_name.toLowerCase().includes(query),
  );
}

function allVisibleOkChecked(): boolean {
  const scoped = visibleItems().filter((item) => item.status === "ok");
  return scoped.length > 0 && scoped.every((item) => state.checked.has(item.id));
}

function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

function formatTime(date: Date): string {
  return date.toTimeString().slice(0, 8);
}

function elidePath(path: string, max = 46): string {
  if (path.length <= max) return path;
  const head = Math.ceil((max - 1) / 2);
  const tail = Math.floor((max - 1) / 2);
  return `${path.slice(0, head)}…${path.slice(-tail)}`;
}

function buildMode(): RenameMode {
  switch (state.mode) {
    case "find_replace":
      return { type: "find_replace", params: { ...state.findReplace } };
    case "sequential":
      return { type: "sequential", params: { ...state.sequential } };
    case "pattern":
      return { type: "pattern", params: { ...state.pattern } };
    case "suffix":
      return { type: "suffix", params: { ...state.suffix } };
  }
}

// ─── 主题 ────────────────────────────────────────────────

function applyTheme(theme: Theme): void {
  document.documentElement.setAttribute("data-theme", theme);
  if (!ui) return;
  ui.themeToggle.replaceChildren(icon(theme === "dark" ? "sun" : "moon", 16));
  const label = theme === "dark" ? "切换到浅色主题" : "切换到深色主题";
  ui.themeToggle.setAttribute("aria-label", label);
  ui.themeToggle.title = label;
}

function toggleTheme(): void {
  state.theme = state.theme === "dark" ? "light" : "dark";
  state.themeManual = true;
  applyTheme(state.theme);
  persist();
}

function bindSystemTheme(): void {
  if (typeof window.matchMedia !== "function") return;
  const media = window.matchMedia("(prefers-color-scheme: light)");
  media.addEventListener("change", (event: MediaQueryListEvent) => {
    if (state.themeManual) return;
    state.theme = event.matches ? "light" : "dark";
    applyTheme(state.theme);
  });
}

// ─── 日志 ────────────────────────────────────────────────

function addLog(kind: LogKind, text: string): void {
  state.logs.push({
    time: formatTime(new Date()),
    text,
    ok: kind !== "error",
    kind,
  });
  if (state.logs.length > MAX_LOGS) {
    state.logs.splice(0, state.logs.length - MAX_LOGS);
  }
  renderLogs();
}

function renderLogs(): void {
  if (!ui) return;
  ui.logBody.replaceChildren();
  if (state.logs.length === 0) {
    ui.logBody.append(el("div", { class: "log-empty", text: "暂无操作记录" }));
  } else {
    for (const entry of state.logs) {
      ui.logBody.append(
        el(
          "div",
          { class: `log-entry log-${entry.kind}` },
          el("span", { class: "log-time", text: entry.time }),
          el("span", { class: "log-text", text: entry.text }),
        ),
      );
    }
    ui.logBody.scrollTop = ui.logBody.scrollHeight;
  }
  ui.logCount.textContent =
    state.logs.length > 0 ? `${state.logs.length} 条` : "";
}

function toggleLogPanel(): void {
  state.logCollapsed = !state.logCollapsed;
  ui.logPanel.classList.toggle("collapsed", state.logCollapsed);
  ui.logToggleBtn.setAttribute("aria-expanded", String(!state.logCollapsed));
  const label = state.logCollapsed ? "展开操作日志" : "折叠操作日志";
  ui.logToggleBtn.setAttribute("aria-label", label);
  ui.logToggleBtn.title = label;
  ui.logToggleBtn.replaceChildren(
    icon(state.logCollapsed ? "chevron-up" : "chevron-down", 14),
  );
}

// ─── Toast ───────────────────────────────────────────────

function removeToast(box: HTMLElement): void {
  const timer = toastTimers.get(box);
  if (timer !== undefined) {
    window.clearTimeout(timer);
    toastTimers.delete(box);
  }
  box.remove();
}

function dismissToasts(): void {
  for (const box of Array.from(ui.toastContainer.children)) {
    if (box instanceof HTMLElement) removeToast(box);
  }
}

function showToast(kind: LogKind, message: string): void {
  if (!ui) return;
  const iconName: IconName =
    kind === "success" ? "check-circle" : kind === "error" ? "alert-circle" : "info";
  const box = el(
    "div",
    {
      class: `toast toast-${kind}`,
      attrs: { role: kind === "error" ? "alert" : "status" },
    },
    el("span", { class: "toast-icon" }, icon(iconName, 16)),
    el("span", { class: "toast-msg", text: message }),
    el(
      "button",
      {
        class: "toast-close",
        attrs: { type: "button", "aria-label": "关闭提示" },
        on: { click: () => removeToast(box) },
      },
      icon("x", 13),
    ),
  );
  while (ui.toastContainer.childElementCount >= MAX_TOASTS) {
    const oldest = ui.toastContainer.firstElementChild;
    if (!(oldest instanceof HTMLElement)) break;
    removeToast(oldest);
  }
  ui.toastContainer.append(box);
  const timer = window.setTimeout(() => removeToast(box), TOAST_DURATION);
  toastTimers.set(box, timer);
}

function reportError(prefix: string, error: unknown): void {
  const message = `${prefix}：${errorMessage(error)}`;
  addLog("error", message);
  showToast("error", message);
}

// ─── 构建 App 骨架 ───────────────────────────────────────

function buildModeTab(mode: ModeType): HTMLButtonElement {
  const active = state.mode === mode;
  return el(
    "button",
    {
      class: `mode-tab${active ? " active" : ""}`,
      id: `mode-tab-${mode}`,
      attrs: {
        type: "button",
        "aria-pressed": String(active),
      },
      on: { click: () => switchMode(mode) },
    },
    el("span", { class: "mode-tab-icon" }, icon(MODE_ICONS[mode], 18)),
    el("span", { class: "mode-tab-label", text: MODE_LABELS[mode] }),
  );
}

function buildFindReplacePanel(): HTMLElement {
  type BooleanParamKey =
    | "use_regex"
    | "case_sensitive"
    | "whole_word"
    | "first_only"
    | "include_extension";
  const options: Array<[string, BooleanParamKey]> = [
    ["使用正则表达式", "use_regex"],
    ["区分大小写", "case_sensitive"],
    ["全词匹配", "whole_word"],
    ["仅替换首个", "first_only"],
    ["包含扩展名", "include_extension"],
  ];
  return el(
    "div",
    { class: "mode-panel" },
    buildTextField({
      label: "查找内容",
      value: state.findReplace.find,
      placeholder: "要查找的文本",
      onInput: (value) => {
        state.findReplace.find = value;
        persist();
      },
    }),
    buildTextField({
      label: "替换为",
      value: state.findReplace.replace,
      placeholder: "替换后的文本（可留空）",
      onInput: (value) => {
        state.findReplace.replace = value;
        persist();
      },
    }),
    el(
      "div",
      { class: "opt-grid" },
      ...options.map(([label, key]) =>
        buildCheckbox(label, state.findReplace[key], (checked) => {
          state.findReplace[key] = checked;
          persist();
        }),
      ),
    ),
    el("p", {
      class: "hint",
      text: "默认只替换主文件名；正则模式下替换文本支持 $1 等捕获组。",
    }),
  );
}

function buildSequentialPanel(): HTMLElement {
  return el(
    "div",
    { class: "mode-panel" },
    el(
      "div",
      { class: "field-grid" },
      buildTextField({
        label: "前缀",
        value: state.sequential.prefix,
        placeholder: "file",
        mono: true,
        onInput: (value) => {
          state.sequential.prefix = value;
          persist();
        },
      }),
      buildTextField({
        label: "连接符",
        value: state.sequential.separator,
        placeholder: "_",
        mono: true,
        onInput: (value) => {
          state.sequential.separator = value;
          persist();
        },
      }),
    ),
    el(
      "div",
      { class: "field-grid" },
      buildNumberField({
        label: "起始编号",
        value: state.sequential.start,
        min: 0,
        onChange: (value) => {
          state.sequential.start = value;
          persist();
        },
      }),
      buildNumberField({
        label: "步长",
        value: state.sequential.step,
        min: 1,
        onChange: (value) => {
          state.sequential.step = value;
          persist();
        },
      }),
    ),
    el(
      "div",
      { class: "field-grid" },
      buildNumberField({
        label: "补零位数（0=自动）",
        value: state.sequential.pad_width,
        min: 0,
        max: 20,
        onChange: (value) => {
          state.sequential.pad_width = value;
          persist();
        },
      }),
      buildTextField({
        label: "数字后缀",
        value: state.sequential.suffix,
        placeholder: "可留空",
        mono: true,
        onInput: (value) => {
          state.sequential.suffix = value;
          persist();
        },
      }),
    ),
    buildScopeControl(state.sequential.scope, (scope) => {
      state.sequential.scope = scope;
      persist();
    }),
    el("p", {
      class: "hint",
      text: "每目录独立编号或全局连续；补零位数 0 表示按编号范围自动补零。",
    }),
  );
}

function buildPatternPanel(): HTMLElement {
  return el(
    "div",
    { class: "mode-panel" },
    buildTextField({
      label: "文件名模板",
      value: state.pattern.template,
      placeholder: "{date}_{name}_{index}",
      mono: true,
      onInput: (value) => {
        state.pattern.template = value;
        persist();
      },
    }),
    el("p", {
      class: "hint",
      text: "{name} 原名 · {original} 全名 · {ext} 扩展名 · {index} 编号 · {date}/{time} 修改时间 · {parent} 目录名 · {{ }} 转义花括号",
    }),
    el(
      "div",
      { class: "field-grid" },
      buildNumberField({
        label: "起始编号",
        value: state.pattern.start,
        min: 0,
        onChange: (value) => {
          state.pattern.start = value;
          persist();
        },
      }),
      buildNumberField({
        label: "步长",
        value: state.pattern.step,
        min: 1,
        onChange: (value) => {
          state.pattern.step = value;
          persist();
        },
      }),
    ),
    buildNumberField({
      label: "补零位数（0=自动）",
      value: state.pattern.pad_width,
      min: 0,
      max: 20,
      onChange: (value) => {
        state.pattern.pad_width = value;
        persist();
      },
    }),
    buildScopeControl(state.pattern.scope, (scope) => {
      state.pattern.scope = scope;
      persist();
    }),
    el("p", {
      class: "hint",
      text: "模板未包含 {ext} 或 {original} 时自动补回原扩展名。",
    }),
  );
}

function buildSuffixPanel(): HTMLElement {
  return el(
    "div",
    { class: "mode-panel" },
    buildTextField({
      label: "目标后缀",
      value: state.suffix.target_suffix,
      placeholder: "例如 jpg（不带点号）",
      mono: true,
      onInput: (value) => {
        state.suffix.target_suffix = value;
        persist();
      },
    }),
    el(
      "div",
      { class: "opt-stack" },
      buildCheckbox("强制转换所有扩展名", state.suffix.force_all, (checked) => {
        state.suffix.force_all = checked;
        persist();
      }),
      buildCheckbox("将后缀转为小写", state.suffix.lowercase, (checked) => {
        state.suffix.lowercase = checked;
        persist();
      }),
    ),
    el("p", {
      class: "hint",
      text: "仅替换最后一个扩展名；未强制转换时只给没有扩展名的文件追加。",
    }),
  );
}

function buildApp(root: HTMLElement): UiRefs {
  // ── Header ──
  const statusDot = el("span", { class: "status-dot" });
  const statusText = el("span", {
    class: "status-text",
    id: "status-text",
    text: "就绪",
  });
  const spinner = el("span", {
    class: "spinner",
    id: "spinner",
    attrs: { "aria-hidden": "true" },
  });
  const statusPill = el(
    "div",
    {
      class: "status-pill",
      id: "status-pill",
      attrs: { "data-tone": "ready", role: "status", "aria-live": "polite" },
    },
    statusDot,
    spinner,
    statusText,
  );
  const themeToggle = el("button", {
    class: "icon-btn",
    id: "btn-theme",
    attrs: { type: "button", "aria-label": "切换主题" },
    on: { click: () => toggleTheme() },
  });
  const header = el(
    "header",
    { class: "app-header", id: "app-header" },
    el(
      "div",
      { class: "brand" },
      el("div", { class: "brand-logo" }, icon("pen", 16)),
      el(
        "div",
        { class: "brand-text" },
        el("div", { class: "brand-title", text: "批量重命名工具" }),
        el("div", { class: "brand-sub", text: "本地批量重命名" }),
      ),
    ),
    el("div", { class: "header-right" }, statusPill, themeToggle),
  );

  // ── Sidebar：目标文件夹 ──
  const pathInput = el("input", {
    class: "input path-input",
    id: "root-path",
    attrs: {
      type: "text",
      placeholder: "选择或输入文件夹路径",
      "aria-label": "目标文件夹路径",
      spellcheck: "false",
      autocomplete: "off",
    },
  });
  pathInput.addEventListener("input", () => {
    state.rootPath = pathInput.value;
    pathInput.title = state.rootPath;
    updateActions();
  });
  pathInput.addEventListener("focus", () => {
    if (pathInput.dataset.elided === "1") {
      pathInput.value = state.rootPath;
      delete pathInput.dataset.elided;
    }
  });
  pathInput.addEventListener("blur", () => {
    // 失焦后按输入框实际宽度做中间省略，聚焦时恢复完整路径便于编辑
    const available = Math.max(
      18,
      Math.floor(pathInput.clientWidth / 7.2),
    );
    pathInput.value = elidePath(state.rootPath.trim(), available);
    pathInput.dataset.elided = "1";
    pathInput.title = state.rootPath;
  });

  const browseBtn = el(
    "button",
    {
      class: "btn btn-browse",
      id: "btn-browse",
      attrs: { type: "button", title: "浏览文件夹" },
      on: { click: () => void onBrowse() },
    },
    icon("folder-open", 14),
    "浏览",
  );

  const recursiveSwitch = buildSwitch({
    label: "包含子目录",
    checked: state.recursive,
    onChange: (checked) => {
      state.recursive = checked;
      persist();
    },
  });

  recursiveSwitch.input.id = "recursive";

  const folderSection = buildSection(
    "目标文件夹",
    el("div", { class: "path-row" }, pathInput, browseBtn),
    recursiveSwitch.row,
  );

  // ── Sidebar：命名模式 ──
  const modeTabs = {} as Record<ModeType, HTMLButtonElement>;
  for (const mode of MODE_TYPES) modeTabs[mode] = buildModeTab(mode);
  const modeGrid = el(
    "div",
    { class: "mode-grid" },
    ...MODE_TYPES.map((mode) => modeTabs[mode]),
  );

  const modePanels = {
    find_replace: buildFindReplacePanel(),
    sequential: buildSequentialPanel(),
    pattern: buildPatternPanel(),
    suffix: buildSuffixPanel(),
  } satisfies Record<ModeType, HTMLElement>;
  for (const mode of MODE_TYPES) {
    modePanels[mode].id = `mode-panel-${mode}`;
    modePanels[mode].hidden = state.mode !== mode;
  }
  const modeSection = buildSection(
    "命名模式",
    modeGrid,
    ...MODE_TYPES.map((mode) => modePanels[mode]),
  );

  // ── Sidebar：过滤与排序 ──
  const filterInput = el("input", {
    class: "input",
    id: "filter-exts",
    attrs: {
      type: "text",
      placeholder: "png,jpg,mp4（留空 = 全部）",
      "aria-label": "扩展名过滤",
      spellcheck: "false",
      autocomplete: "off",
    },
    props: { value: state.filterExts },
  });
  filterInput.addEventListener("input", () => {
    state.filterExts = filterInput.value;
    persist();
  });
  const filterSection = buildSection(
    "过滤与排序",
    el(
      "label",
      { class: "field" },
      el("span", { class: "field-label", text: "扩展名过滤" }),
      filterInput,
    ),
    buildSelectField({
      label: "排序方式",
      value: state.sortBy,
      options: [
        { value: "name", label: "文件名（自然）" },
        { value: "modified", label: "修改时间" },
        { value: "created", label: "创建时间" },
      ],
      onChange: (value) => {
        state.sortBy = value as SortBy;
        persist();
      },
    }),
  );

  // ── Sidebar：底部操作区 ──
  const previewBtn = el(
    "button",
    {
      class: "btn btn-primary btn-block",
      id: "btn-preview",
      attrs: { type: "button", title: "预览变更（⌘/Ctrl+Enter）" },
      on: { click: () => void onPreview() },
    },
    icon("play", 14),
    "预览变更",
  );
  const executeBtn = el(
    "button",
    {
      class: "btn btn-danger",
      id: "btn-execute",
      attrs: { type: "button", title: "执行重命名（⌘/Ctrl+Shift+Enter）" },
      on: { click: () => void onExecute() },
    },
    "执行重命名",
  );
  const cancelBtn = el(
    "button",
    {
      class: "btn btn-ghost",
      id: "btn-cancel",
      attrs: { type: "button" },
      on: { click: () => void onCancel() },
      text: "取消",
    },
  );
  const undoBtn = el(
    "button",
    {
      class: "btn btn-ghost",
      id: "btn-undo",
      attrs: { type: "button" },
      on: { click: () => void onUndo() },
    },
    icon("undo", 14),
    "撤销上次",
  );
  const clearBtn = el(
    "button",
    {
      class: "btn btn-ghost",
      id: "btn-clear",
      attrs: { type: "button" },
      on: { click: () => onClear() },
      text: "清空",
    },
  );
  const selectAllBtn = el("button", {
    class: "btn btn-ghost",
    id: "btn-select-all",
    attrs: { type: "button" },
    on: { click: () => toggleSelectAll() },
    text: "全选",
  });
  const fileCount = el("div", {
    class: "file-count",
    id: "file-count",
    text: "尚未预览文件",
    attrs: { "aria-live": "polite" },
  });

  const sidebarActions = el(
    "div",
    { class: "sidebar-actions" },
    previewBtn,
    el("div", { class: "action-row" }, executeBtn, cancelBtn),
    el("div", { class: "action-row three" }, undoBtn, clearBtn, selectAllBtn),
    fileCount,
  );

  const sidebarScroll = el(
    "div",
    { class: "sidebar-scroll" },
    folderSection,
    modeSection,
    filterSection,
  );
  const sidebar = el(
    "aside",
    { class: "sidebar", id: "sidebar" },
    sidebarScroll,
    sidebarActions,
  );

  // ── Main：进度条 ──
  const progressFill = el("div", { class: "progress-fill", id: "progress-fill" });
  const progress = el(
    "div",
    { class: "progress", id: "progress-bar" },
    progressFill,
  );

  // ── Main：工具栏 ──
  const chipDefs: Array<[Status, string]> = [
    ["ok", "待执行"],
    ["conflict", "冲突"],
    ["unchanged", "无变化"],
  ];
  const chipCounts = {} as Record<Status, HTMLElement>;
  const chips = chipDefs.map(([status, label]) => {
    const count = el("span", { class: "chip-count", text: "0" });
    chipCounts[status] = count;
    return el(
      "span",
      { class: `chip chip-${status}` },
      el("span", { class: `dot dot-${status}` }),
      el("span", { class: "chip-label", text: label }),
      count,
    );
  });

  const searchInput = el("input", {
    class: "search-input",
    id: "search-input",
    attrs: {
      type: "search",
      placeholder: "搜索路径或文件名",
      "aria-label": "搜索路径或文件名",
      spellcheck: "false",
      autocomplete: "off",
    },
  });
  searchInput.addEventListener("input", () => {
    state.search = searchInput.value;
    renderTable();
  });

  const selectAllCheckbox = el("input", {
    class: "check-input",
    id: "select-all",
    attrs: { type: "checkbox", "aria-label": "全选待执行文件" },
    on: { change: () => toggleSelectAll() },
  });
  const selectAll = el(
    "label",
    { class: "select-all" },
    selectAllCheckbox,
    el("span", { text: "全选" }),
  );

  const toolbar = el(
    "div",
    { class: "main-toolbar" },
    el("div", { class: "chips", id: "chips" }, ...chips),
    el(
      "div",
      { class: "toolbar-right" },
      el(
        "label",
        { class: "search" },
        el("span", { class: "search-icon" }, icon("search", 14)),
        searchInput,
      ),
      selectAll,
    ),
  );

  // ── Main：表格 ──
  const headerCheckbox = el("input", {
    class: "check-input",
    attrs: { type: "checkbox", "aria-label": "全选待执行文件" },
    on: { change: () => toggleSelectAll() },
  });
  const tbody = el("tbody", { id: "preview-tbody" });
  const table = el(
    "table",
    { class: "preview-table", id: "preview-table" },
    el(
      "thead",
      {},
      el(
        "tr",
        {},
        el("th", { class: "col-check" }, headerCheckbox),
        el("th", { class: "col-path", text: "相对路径" }),
        el("th", { class: "col-name", text: "原文件名" }),
        el("th", { class: "col-name", text: "新文件名" }),
        el("th", { class: "col-status", text: "状态" }),
      ),
    ),
    tbody,
  );

  const emptyIcon = el("div", { class: "empty-icon" }, icon("file", 22));
  const emptyTitle = el("p", { class: "empty-title", text: "还没有预览文件" });
  const emptyHint = el("p", {
    class: "empty-hint",
    text: "选择目标文件夹、配置命名模式后，点击「预览变更」查看结果。",
  });
  const emptyState = el(
    "div",
    { class: "empty-state", id: "empty-state" },
    emptyIcon,
    emptyTitle,
    emptyHint,
  );
  const tableScroll = el("div", { class: "table-scroll" }, table, emptyState);

  // ── Main：日志面板 ──
  const logBody = el("div", { class: "log-body", id: "log-body" });
  const logCount = el("span", { class: "log-count", id: "log-count" });
  const logToggleBtn = el("button", {
    class: "icon-btn",
    id: "btn-log-toggle",
    attrs: {
      type: "button",
      "aria-label": "折叠操作日志",
      "aria-expanded": "true",
    },
    on: { click: () => toggleLogPanel() },
  });
  const logPanel = el(
    "div",
    { class: "log-panel", id: "log-panel" },
    el(
      "div",
      { class: "log-head" },
      el(
        "span",
        { class: "log-title" },
        icon("terminal", 13),
        el("span", { text: "操作日志" }),
      ),
      logCount,
      el(
        "div",
        { class: "log-actions" },
        el(
          "button",
          {
            class: "btn btn-ghost btn-sm",
            id: "btn-log-clear",
            attrs: { type: "button" },
            on: {
              click: () => {
                state.logs = [];
                renderLogs();
              },
            },
          },
          "清空",
        ),
        logToggleBtn,
      ),
    ),
    logBody,
  );

  const main = el(
    "div",
    { class: "main", id: "main" },
    progress,
    toolbar,
    tableScroll,
    logPanel,
  );
  const workspace = el(
    "div",
    { class: "workspace", id: "workspace" },
    sidebar,
    main,
  );
  root.replaceChildren(header, workspace);

  const toastContainer = el("div", {
    class: "toast-container",
    id: "toast-container",
    attrs: { role: "log", "aria-live": "polite", "aria-label": "操作提示" },
  });
  document.body.append(toastContainer);

  return {
    statusPill,
    statusText,
    spinner,
    themeToggle,
    pathInput,
    browseBtn,
    recursiveToggle: recursiveSwitch.input,
    modeTabs,
    modePanels,
    previewBtn,
    executeBtn,
    cancelBtn,
    undoBtn,
    clearBtn,
    selectAllBtn,
    fileCount,
    chipCounts,
    searchInput,
    selectAllCheckbox,
    headerCheckbox,
    progress,
    progressFill,
    table,
    tbody,
    tableScroll,
    emptyState,
    emptyIcon,
    emptyTitle,
    emptyHint,
    logPanel,
    logBody,
    logCount,
    logToggleBtn,
    toastContainer,
    rowMap: new Map<number, RowRef>(),
  };
}

// ─── 模式切换 ────────────────────────────────────────────

function switchMode(mode: ModeType): void {
  if (state.mode === mode) return;
  state.mode = mode;
  persist();
  for (const type of MODE_TYPES) {
    const active = type === mode;
    ui.modeTabs[type].classList.toggle("active", active);
    ui.modeTabs[type].setAttribute("aria-pressed", String(active));
    ui.modePanels[type].hidden = !active;
  }
}

// ─── UI 同步 ─────────────────────────────────────────────

function updateHeader(): void {
  let text = "就绪";
  let tone = "ready";
  if (state.activity === "preview") {
    text = "加载中…";
    tone = "busy";
  } else if (state.activity === "execute") {
    text = state.progress
      ? `执行中 ${state.progress.current}/${state.progress.total}`
      : "执行中…";
    tone = "busy";
  } else if (state.activity === "undo") {
    text = "撤销中…";
    tone = "busy";
  } else if (state.completed) {
    text = "执行完成";
    tone = "done";
  }
  ui.statusText.textContent = text;
  ui.statusPill.dataset.tone = tone;
  ui.spinner.classList.toggle("active", tone === "busy");
}

function updateProgress(): void {
  const busy = state.activity !== "idle";
  ui.progress.classList.toggle("is-active", busy);
  const determinate =
    state.activity === "execute" &&
    state.progress !== null &&
    state.progress.total > 0;
  if (determinate && state.progress !== null) {
    ui.progressFill.classList.remove("indeterminate");
    const percent = Math.min(
      100,
      Math.max(0, (state.progress.current / state.progress.total) * 100),
    );
    ui.progressFill.style.width = `${percent}%`;
  } else if (busy) {
    ui.progressFill.classList.add("indeterminate");
    ui.progressFill.style.width = "";
  } else {
    ui.progressFill.classList.remove("indeterminate");
    ui.progressFill.style.width = "0%";
  }
}

function updateChips(): void {
  const counts = countStatuses(state.preview);
  ui.chipCounts.ok.textContent = String(counts.ok);
  ui.chipCounts.conflict.textContent = String(counts.conflict);
  ui.chipCounts.unchanged.textContent = String(counts.unchanged);
}

function updateFileCount(): void {
  const total = state.preview.length;
  if (total === 0) {
    ui.fileCount.textContent = "尚未预览文件";
    return;
  }
  const selected = selectedOkItems().length;
  const conflicts = countStatuses(state.preview).conflict;
  const suffix = conflicts > 0 ? ` · 冲突 ${conflicts}` : "";
  ui.fileCount.textContent = `共 ${total} 个文件 · 已选 ${selected}${suffix}`;
}

function updateActions(): void {
  const busy = state.activity !== "idle";
  const selected = selectedOkItems().length;
  const okCount = countStatuses(state.preview).ok;

  ui.previewBtn.disabled = busy || state.rootPath.trim().length === 0;
  ui.executeBtn.disabled = busy || selected === 0;
  ui.executeBtn.title =
    selected === 0
      ? "请先预览并勾选待执行文件"
      : `将重命名 ${selected} 个文件（⌘/Ctrl+Shift+Enter）`;
  ui.cancelBtn.disabled = state.activity !== "execute";
  ui.undoBtn.disabled = busy || !state.undo.available;
  ui.undoBtn.title = state.undo.available
    ? `撤销上次重命名（${state.undo.count} 个文件）`
    : "没有可撤销的操作";
  ui.browseBtn.disabled = busy;
  ui.clearBtn.disabled = busy;
  ui.selectAllBtn.disabled = okCount === 0;
  ui.selectAllBtn.textContent = allVisibleOkChecked() ? "取消全选" : "全选";
}

function syncSelectAllCheckboxes(): void {
  const scoped = visibleItems().filter((item) => item.status === "ok");
  const all = scoped.length > 0 && scoped.every((item) => state.checked.has(item.id));
  const some = scoped.some((item) => state.checked.has(item.id));
  for (const checkbox of [ui.selectAllCheckbox, ui.headerCheckbox]) {
    checkbox.checked = all;
    checkbox.indeterminate = !all && some;
    checkbox.disabled = scoped.length === 0;
  }
}

function refreshChrome(): void {
  updateHeader();
  updateProgress();
  updateChips();
  updateFileCount();
  updateActions();
}

function syncSelectionUI(): void {
  for (const [id, ref] of ui.rowMap) {
    const checked = state.checked.has(id);
    ref.checkbox.checked = checked;
    ref.row.classList.toggle("row-unchecked", !checked);
  }
  syncSelectAllCheckboxes();
  updateFileCount();
  updateActions();
}

function renderEmpty(kind: "initial" | "no-match"): void {
  ui.table.hidden = true;
  ui.emptyState.hidden = false;
  if (kind === "initial") {
    ui.emptyIcon.replaceChildren(icon("file", 22));
    ui.emptyTitle.textContent = "还没有预览文件";
    ui.emptyHint.textContent =
      "选择目标文件夹、配置命名模式后，点击「预览变更」查看结果。";
  } else {
    ui.emptyIcon.replaceChildren(icon("search", 22));
    ui.emptyTitle.textContent = "没有匹配的文件";
    ui.emptyHint.textContent = `没有找到包含「${state.search.trim()}」的文件，试试清空搜索或调整过滤条件。`;
  }
}

function renderTable(): void {
  ui.tbody.replaceChildren();
  ui.rowMap.clear();

  if (state.preview.length === 0) {
    renderEmpty("initial");
    syncSelectAllCheckboxes();
    updateChips();
    updateFileCount();
    updateActions();
    return;
  }

  const items = visibleItems();
  if (items.length === 0) {
    renderEmpty("no-match");
    syncSelectAllCheckboxes();
    updateChips();
    updateFileCount();
    updateActions();
    return;
  }

  ui.table.hidden = false;
  ui.emptyState.hidden = true;

  for (const item of items) {
    const checked = state.checked.has(item.id);
    const checkbox = el("input", {
      class: "check-input",
      attrs: {
        type: "checkbox",
        "aria-label": `选择 ${item.old_name}`,
      },
      props: { checked, disabled: item.status !== "ok" },
    });
    checkbox.addEventListener("change", () => {
      if (checkbox.checked) state.checked.add(item.id);
      else state.checked.delete(item.id);
      syncSelectionUI();
    });

    const pathCell = el(
      "td",
      { class: "col-path", title: item.relative_path || "根目录" },
      el(
        "span",
        { class: "cell-path" },
        el("span", { class: "path-icon" }, icon("folder", 12)),
        el("span", {
          class: "path-text",
          text: item.relative_path || "./",
        }),
      ),
    );

    const oldCell = el("td", {
      class: "col-name cell-name",
      text: item.old_name,
      title: item.old_name,
    });

    const newClass =
      item.status === "conflict"
        ? "col-name cell-name new-name is-conflict"
        : item.status === "unchanged"
          ? "col-name cell-name new-name is-unchanged"
          : "col-name cell-name new-name";
    const newCell = el("td", {
      class: newClass,
      text: item.new_name || "—",
      title: item.new_name,
    });

    const statusCell = el(
      "td",
      { class: "col-status" },
      el("span", {
        class: `badge badge-${item.status}`,
        text: STATUS_LABELS[item.status],
        attrs: item.reason
          ? {
              title: item.reason,
              "aria-label": `状态：${STATUS_LABELS[item.status]}，原因：${item.reason}`,
            }
          : {},
      }),
    );

    const row = el(
      "tr",
      {
        class:
          "preview-row" +
          (item.status === "conflict" ? " row-conflict" : "") +
          (checked ? "" : " row-unchecked"),
      },
      el("td", { class: "col-check" }, checkbox),
      pathCell,
      oldCell,
      newCell,
      statusCell,
    );
    ui.rowMap.set(item.id, { row, checkbox });
    ui.tbody.append(row);
  }

  syncSelectAllCheckboxes();
  updateChips();
  updateFileCount();
  updateActions();
}

// ─── 用户操作：选择文件夹 / 预览 / 执行 / 取消 / 撤销 / 清空 ──

async function onBrowse(): Promise<void> {
  if (state.activity !== "idle") return;
  try {
    const folder = await selectFolder();
    if (!folder) return;
    state.rootPath = folder;
    ui.pathInput.value = folder;
    ui.pathInput.title = folder;
    delete ui.pathInput.dataset.elided;
    updateActions();
  } catch (error) {
    reportError("选择文件夹失败", error);
  }
}

interface PreviewOptions {
  silent?: boolean;
}

async function onPreview(options: PreviewOptions = {}): Promise<void> {
  if (state.activity !== "idle") return;
  const rootPath = state.rootPath.trim();
  if (!rootPath) {
    if (!options.silent) showToast("info", "请先选择目标文件夹");
    return;
  }

  beginActivity("preview");
  try {
    const items = await getPreview(
      rootPath,
      state.recursive,
      buildMode(),
      state.filterExts.trim() || null,
      state.sortBy,
    );
    state.preview = items;
    state.checked = new Set(
      items.filter((item) => item.status === "ok").map((item) => item.id),
    );
    const counts = countStatuses(items);
    addLog(
      "success",
      `预览完成：共 ${items.length} 个文件，待执行 ${counts.ok}、冲突 ${counts.conflict}、无变化 ${counts.unchanged}`,
    );
    renderTable();
  } catch (error) {
    state.preview = [];
    state.checked.clear();
    renderTable();
    reportError("预览失败", error);
  } finally {
    endActivity();
  }
}

async function onExecute(): Promise<void> {
  if (state.activity !== "idle") return;
  const rootPath = state.rootPath.trim();
  if (!rootPath) {
    showToast("info", "请先选择目标文件夹");
    return;
  }
  const selected = selectedOkItems();
  if (selected.length === 0) {
    showToast("info", "没有可执行的文件");
    addLog("info", "没有勾选状态为「待执行」的文件");
    return;
  }

  const ids = selected.map((item) => item.id);
  const names = new Map(state.preview.map((item) => [item.id, item.old_name]));
  // 执行期间若用户改动参数，也不会影响本次请求
  const mode = buildMode();
  const recursive = state.recursive;
  const filterExts = state.filterExts.trim() || null;
  const sortBy = state.sortBy;

  beginActivity("execute");
  addLog("info", `开始执行重命名：${ids.length} 个文件`);

  let succeeded = false;
  try {
    const results = await executeRename(
      rootPath,
      recursive,
      mode,
      filterExts,
      sortBy,
      ids,
    );
    const okCount = results.filter((result) => result.success).length;
    const failed = results.filter((result) => !result.success);
    for (const result of failed) {
      const name = names.get(result.id) ?? `#${result.id}`;
      addLog("error", `失败 · ${name}：${result.message}`);
    }
    if (failed.length === 0) {
      addLog("success", `执行完成：${okCount} 个文件重命名成功`);
      showToast("success", `执行完成：${okCount} 个文件已重命名`);
    } else {
      addLog(
        "error",
        `执行完成：${okCount} 成功，${failed.length} 失败`,
      );
      showToast(
        "error",
        `执行完成：${okCount} 成功 / ${failed.length} 失败`,
      );
    }
    succeeded = true;
  } catch (error) {
    const message = errorMessage(error);
    if (message.includes("取消")) {
      addLog("info", "执行已取消，已完成的文件保留重命名结果");
      showToast("info", "已取消执行");
    } else {
      addLog("error", `执行中断：${message}`);
      showToast("error", `执行失败：${message}`);
    }
  } finally {
    // 旧预览在磁盘上已失效，先取消勾选避免重复执行
    state.checked.clear();
    endActivity();
    await refreshUndoStatus(false);
    await onPreview({ silent: true });
    if (succeeded) {
      state.completed = true;
      scheduleCompletedReset();
      updateHeader();
    }
  }
}

async function onUndo(): Promise<void> {
  if (state.activity !== "idle") return;
  if (!state.undo.available) {
    showToast("info", "没有可撤销的操作");
    return;
  }
  beginActivity("undo");
  addLog("info", `开始撤销上次重命名（${state.undo.count} 个文件）`);
  try {
    const results = await undoLastRename();
    const okCount = results.filter((result) => result.success).length;
    const failed = results.filter((result) => !result.success);
    for (const result of failed) {
      addLog("error", `撤销失败 · #${result.id}：${result.message}`);
    }
    if (failed.length === 0) {
      addLog("success", `撤销完成：${okCount} 个文件已还原`);
      showToast("success", `撤销完成：${okCount} 个文件已还原`);
    } else {
      showToast(
        "error",
        `撤销完成：${okCount} 成功 / ${failed.length} 失败`,
      );
    }
  } catch (error) {
    reportError("撤销失败", error);
  } finally {
    state.checked.clear();
    endActivity();
    await refreshUndoStatus(false);
    await onPreview({ silent: true });
  }
}

async function onCancel(): Promise<void> {
  if (state.activity !== "execute") return;
  try {
    await cancelOperation();
    addLog("info", "已请求取消，等待当前文件处理完成后停止…");
  } catch (error) {
    reportError("取消失败", error);
  }
}

function onClear(): void {
  if (state.activity !== "idle") return;
  state.preview = [];
  state.checked.clear();
  state.search = "";
  ui.searchInput.value = "";
  state.progress = null;
  state.completed = false;
  clearCompletedTimer();
  addLog("info", "已清空预览列表与勾选");
  renderTable();
  refreshChrome();
}

function toggleSelectAll(): void {
  // 本地选择操作，忙碌期间也允许（不会触发后端请求）
  const scoped = visibleItems().filter((item) => item.status === "ok");
  if (scoped.length === 0) return;
  const allChecked = scoped.every((item) => state.checked.has(item.id));
  for (const item of scoped) {
    if (allChecked) state.checked.delete(item.id);
    else state.checked.add(item.id);
  }
  syncSelectionUI();
}

async function refreshUndoStatus(notify: boolean): Promise<void> {
  try {
    state.undo = await getUndoStatus();
    updateActions();
  } catch (error) {
    state.undo = { available: false, count: 0 };
    updateActions();
    if (notify) reportError("获取撤销状态失败", error);
  }
}

// ─── 活动状态管理 ────────────────────────────────────────

function beginActivity(activity: Activity): void {
  state.activity = activity;
  state.progress = null;
  state.completed = false;
  clearCompletedTimer();
  refreshChrome();
}

function endActivity(): void {
  state.activity = "idle";
  state.progress = null;
  refreshChrome();
}

function scheduleCompletedReset(): void {
  clearCompletedTimer();
  state.completedTimer = window.setTimeout(() => {
    state.completedTimer = null;
    state.completed = false;
    updateHeader();
  }, COMPLETED_DURATION);
}

function clearCompletedTimer(): void {
  if (state.completedTimer !== null) {
    window.clearTimeout(state.completedTimer);
    state.completedTimer = null;
  }
}

// ─── 键盘快捷键 ──────────────────────────────────────────

function onKeydown(event: KeyboardEvent): void {
  if (event.key === "Escape") {
    if (ui.toastContainer.childElementCount > 0) {
      dismissToasts();
      event.preventDefault();
    }
    return;
  }
  const modifier = event.metaKey || event.ctrlKey;
  if (!modifier || event.key !== "Enter") return;
  if (event.shiftKey) {
    if (!ui.executeBtn.disabled) {
      event.preventDefault();
      void onExecute();
    }
  } else if (!ui.previewBtn.disabled) {
    event.preventDefault();
    void onPreview();
  }
}

// ─── 初始化 ──────────────────────────────────────────────

export function init(): void {
  const root = document.getElementById("app");
  if (!root) return;

  state = createInitialState();
  ui = buildApp(root);

  applyTheme(state.theme);
  bindSystemTheme();
  renderLogs();
  renderTable();
  refreshChrome();
  updateProgress();

  document.addEventListener("keydown", onKeydown);

  // 进度事件（无 Tauri 环境下 listen 会 reject，静默忽略）
  void onProgress((payload) => {
    if (state.activity !== "execute") return;
    state.progress = payload;
    updateHeader();
    updateProgress();
  }).catch(() => {
    /* 普通浏览器中无事件系统，忽略即可 */
  });

  // 启动时查询撤销状态（浏览器中静默失败，不影响页面）
  void refreshUndoStatus(false);
}
