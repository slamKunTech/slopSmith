// 临时 hack：降低 Free Play 检测阈值
// 在 Console 里粘贴运行这段代码

(function() {
    // 找到 freeplay.js 的 script 标签
    const scripts = document.querySelectorAll('script');
    let freeplayScript = null;

    for (let s of scripts) {
        if (s.src && s.src.includes('freeplay.js')) {
            freeplayScript = s.src;
            break;
        }
    }

    if (!freeplayScript) {
        console.error('找不到 freeplay.js，无法 patch');
        return;
    }

    console.log('检测到 freeplay.js:', freeplayScript);
    console.log('⚠️  需要修改源码才能降低阈值');
    console.log('');
    console.log('请编辑文件：');
    console.log('  /Users/mac/codes/slopSmith/slopsmith-desktop/resources/slopsmith/static/freeplay.js');
    console.log('');
    console.log('修改第 72-73 行：');
    console.log('  const ABS_FLOOR_DB = -100;  // 原值 -85，改为 -100');
    console.log('  const REL_GATE = 0.05;      // 原值 0.12，改为 0.05');
    console.log('');
    console.log('保存后重启 Slopsmith Desktop');
})();
