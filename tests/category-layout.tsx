import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import Category, { ChannelPage } from "../src/pages/Category";
import { PostsPage } from "../src/pages/Posts";
import { Img } from "../src/components/ui";
import { AppProvider } from "../src/store";
import "../src/index.css";

const items = Array.from({ length: 20 }, (_, i) => ({
  id: 100 - i, title: `Card ${i}`, cover: `https://fixture.invalid/${i}.svg`,
  tags: ["test"], score: i, hot: i, episode_count: 1, total_episodes: 1,
  is_finished: true, is_original: 0, like_count: 0, comment_count: 0, collect_count: 0,
}));
Object.assign(window, { __TAURI_INTERNALS__: { invoke(cmd: string, args: { size?: number } = {}) {
  if (cmd === "app_boot") return Promise.resolve({ user: null, logged_in: false });
  if (cmd === "api_category" || cmd === "api_posts") return Promise.resolve({
    items: items.slice(0, args.size || 20), pagination: { page: 1, pages: 1 },
  });
  if (cmd === "debug_log") return Promise.resolve();
  return Promise.reject(cmd);
} } });
function Fixture() {
  const [width, setWidth] = useState(innerWidth);
  const [src, setSrc] = useState("/fixture-cover.svg?first");
  Object.assign(window, { layoutFixture: { setWidth, setSrc } });
  const mode = new URLSearchParams(location.search).get("mode");
  return <AppProvider><div style={{ position: "relative", width, height: "100vh" }}>
    {mode === "overview" ? <Category wide={width > 640} active />
      : mode === "image" ? <div className="relative w-40 h-40"><Img raw src={src} /></div>
      : mode === "posts" ? <PostsPage wide={width > 640} onBack={() => {}} />
      : <ChannelPage cate="ai-duanju" name="Test channel" wide={width > 640} onBack={() => {}} />}
  </div></AppProvider>;
}
createRoot(document.querySelector("#root")!).render(<React.StrictMode><Fixture /></React.StrictMode>);
