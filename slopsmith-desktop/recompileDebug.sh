#!/bin/bash
# Slopsmith Desktop - 编译并启动调试模式
# Usage: bash compileAndDebug.sh

set -e

cd "$(dirname "$0")"

echo "🔨 编译 TypeScript..."
npm run build:ts

echo ""
echo "🚀 启动 Electron 调试模式..."
echo "   提示：按 Cmd+Option+I 打开开发者工具"
echo "   提示：按 Ctrl+C 停止应用"
echo ""

npx electron .
