// 进度条探针页面（临时，诊断用）：与 probe.js 同构，只换入口函数。

import init, { run_progress_probe } from './wasm/astrcode_webui.js';

async function main() {
  const boot = document.getElementById('boot');
  try {
    await init();
    run_progress_probe();
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
