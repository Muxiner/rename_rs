/** 应用入口：DOM 就绪后启动 App */
import { init } from "./app";

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init, { once: true });
} else {
  init();
}
