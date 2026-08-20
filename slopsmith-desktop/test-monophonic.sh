#!/bin/bash
# 临时测试脚本：使用 JUCE 单音检测（更可靠）
# 如果多音检测问题太严重，可以临时切换到单音模式

echo "⚠️  此脚本将暂时禁用多音检测，使用 JUCE 单音模式"
echo "    多音检测问题：泛音干扰太严重"
echo ""

cd "$(dirname "$0")"

# 备份原文件
if [ ! -f resources/slopsmith/static/freeplay.js.backup ]; then
    cp resources/slopsmith/static/freeplay.js resources/slopsmith/static/freeplay.js.backup
    echo "✓ 已备份 freeplay.js"
fi

# 临时禁用多音检测：强制使用 JUCE 单音
sed -i.tmp 's/if (webAudioOk && analyser) return detectPolyphonic();/\/\/ TEMP: force monophonic\n        \/\/ if (webAudioOk \&\& analyser) return detectPolyphonic();/' resources/slopsmith/static/freeplay.js

echo "✓ 已切换到 JUCE 单音模式"
echo ""
echo "测试完成后恢复："
echo "  mv resources/slopsmith/static/freeplay.js.backup resources/slopsmith/static/freeplay.js"
