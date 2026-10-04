// 浮层探针页面：装载 wasm 后开探针窗口。与 main.js 同构，只换入口函数。

import init, { run_probe } from './wasm/astrcode_webui.js';

async function main() {
  const boot = document.getElementById('boot');

  try {
    await init();
    run_probe();

    requestAnimationFrame(() =>
      requestAnimationFrame(() => {
        boot.remove();
      }),
    );
  } catch (error) {
    window.__bootError = String(error);
    boot.textContent = '装载失败：' + error;
  }
}

main();
