import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";

// 注：不用 <React.StrictMode> —— 开发模式下 StrictMode 会对挂载 effect 做「挂载→卸载→重挂」，
// 导致首页/工坊等页面的数据加载 effect 触发两次（如 workshopRandom 请求两次 = “刷新两次”）。
ReactDOM.createRoot(document.getElementById("root")!).render(<App />);
