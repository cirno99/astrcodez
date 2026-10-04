// astrcode Web UI 页面入口：装载 wasm 后开窗。
//
// 这里只负责「装载」与「把装载失败显示出来」两件事，界面全在 wasm 里。

import init, { run } from './wasm/astrcode_webui.js';

async function main() {
  const boot = document.getElementById('boot');

  try {
    await init();
    run();

    // 窗口的首帧是异步画出来的，等两帧再撤掉遮罩，避免闪一下空白。
    requestAnimationFrame(() =>
      requestAnimationFrame(() => {
        boot.remove();
      }),
    );
  } catch (error) {
    // 装载失败必须落在画面上：此时 wasm 里的界面还不存在，控制台是唯一出口。
    window.__bootError = String(error);
    boot.textContent = '装载失败：' + error;
  }
}

main();
