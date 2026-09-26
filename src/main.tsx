import ReactDOM from "react-dom/client";
import App from "./App";
import { MessageProvider } from "./components/Message";
import { WorkshopFilterProvider } from "./hooks/useWorkshopFilter";
import "./index.css";

// 全屏蔽浏览器默认右键菜单（WKWebView 的 Look Up/Copy 等原生项）：
// 产品界面不出现右键菜单；文本编辑仍可用键盘 ⌘C/⌘V（Edit 菜单）
document.addEventListener("contextmenu", (e) => e.preventDefault());

// 注：不用 <React.StrictMode> —— 开发模式下 StrictMode 会对挂载 effect 做「挂载→卸载→重挂」，
// 导致首页/工坊等页面的数据加载 effect 触发两次（如 workshopRandom 请求两次 = “刷新两次”）。
ReactDOM.createRoot(document.getElementById("root")!).render(
  <MessageProvider>
    <WorkshopFilterProvider>
      <App />
    </WorkshopFilterProvider>
  </MessageProvider>,
);
