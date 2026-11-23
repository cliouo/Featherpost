// 先加载语言运行时：保证任何消息函数被调用前自定义策略已注册。
import "./index";

/** 全部界面消息，调用形如 `m.mailbox_inbox()`；带参数的消息传对象：`m.x({ name })`。 */
export { m } from "./paraglide/messages.js";
