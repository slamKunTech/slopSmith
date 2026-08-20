# 调试 Free Play 电吉他输入问题

## 步骤 1：打开浏览器开发者工具

1. 启动 Slopsmith Desktop
2. 进入 Free Play 模式
3. 打开开发者工具：
   - **macOS**: `Cmd + Option + I`
   - **Windows/Linux**: `Ctrl + Shift + I`

## 步骤 2：检查控制台错误

在 Console 标签页查找以下错误：

### 期望看到的日志：
```
Free Play: Web Audio capture initialized
```

### 可能的错误：
- `NotAllowedError: Permission denied` → 麦克风权限被拒绝
- `NotFoundError: Requested device not found` → 设备不存在
- `Free Play: Web Audio capture failed, falling back to JUCE pitch detection` → Web Audio 失败，使用备用方案

## 步骤 3：在 Console 中运行诊断命令

### 3.1 检查当前状态
```javascript
// 查看 Web Audio 是否正常
console.log('webAudioOk:', window.freeplay?.__debug?.webAudioOk);

// 查看是否有 analyser 节点
console.log('analyser:', window.freeplay?.__debug?.analyser);

// 查看当前选择的设备
console.log('current device:', document.getElementById('fp-device')?.value);
```

### 3.2 查看实时音频电平
```javascript
// 添加临时监控（在 Console 粘贴运行）
if (window.freeplay?.__debug?.analyser) {
    const analyser = window.freeplay.__debug.analyser;
    const dataArray = new Float32Array(analyser.frequencyBinCount);
    
    const checkLevel = () => {
        analyser.getFloatFrequencyData(dataArray);
        const maxDb = Math.max(...dataArray);
        console.log('Max FFT bin dB:', maxDb.toFixed(1), 
                    maxDb > -85 ? '✓ Above threshold' : '✗ Below threshold');
    };
    
    // 每秒打印一次
    const intervalId = setInterval(checkLevel, 1000);
    console.log('Monitoring started. To stop: clearInterval(' + intervalId + ')');
} else {
    console.error('Analyser not available - Web Audio may have failed');
}
```

### 3.3 列出所有音频输入设备
```javascript
navigator.mediaDevices.enumerateDevices().then(devices => {
    const inputs = devices.filter(d => d.kind === 'audioinput');
    console.table(inputs.map(d => ({
        deviceId: d.deviceId.substring(0, 20) + '...',
        label: d.label
    })));
});
```

## 步骤 4：检查音频设备配置

### 4.1 Free Play 界面
在 Free Play 右上角的设备下拉菜单中：
- 查看是否列出了你的音频接口
- 尝试手动选择电吉他所在的设备
- 如果显示 "Default"，尝试切换到具体设备

### 4.2 Audio Engine 面板
1. 点击左下角 Audio Engine 图标
2. 检查 "Input Device" 是否选择了正确的音频接口
3. 查看 Pitch Detection 显示：
   - 如果显示频率和音符 → JUCE 能检测到信号
   - 如果显示 `--` → JUCE 也检测不到

## 步骤 5：临时降低检测阈值（调试用）

如果怀疑阈值过高，可以在 Console 中临时修改：

```javascript
// 注意：这需要修改源码或在运行时 hack
// 打开 /resources/slopsmith/static/freeplay.js:228-231
// 临时降低 ABS_FLOOR_DB 和 REL_GATE

// 如果有权限，可以尝试：
Object.defineProperty(window, 'FREEPLAY_ABS_FLOOR_DB', { value: -100 });
Object.defineProperty(window, 'FREEPLAY_REL_GATE', { value: 0.05 });
```

## 步骤 6：对比 MIDI Guitar for GarageBand 的设置

1. 打开 MIDI Guitar for GarageBand
2. 检查它使用的音频输入设备
3. 在 Slopsmith Free Play 中选择**相同的设备**

## 步骤 7：检查系统音频权限

### macOS:
```bash
# 检查 Slopsmith 的麦克风权限
sqlite3 ~/Library/Application\ Support/com.apple.TCC/TCC.db \
  "SELECT service, client, auth_value FROM access WHERE service='kTCCServiceMicrophone'"
```

或者：
1. 打开 **系统设置** > **隐私与安全性** > **麦克风**
2. 确保 **Slopsmith** 已勾选

## 步骤 8：查看原始频谱数据（高级）

在 Console 运行以下代码来可视化输入信号：

```javascript
if (window.freeplay?.__debug?.analyser) {
    const analyser = window.freeplay.__debug.analyser;
    const canvas = document.createElement('canvas');
    canvas.width = 800;
    canvas.height = 400;
    canvas.style.cssText = 'position:fixed;top:50px;right:50px;z-index:9999;border:2px solid red;';
    document.body.appendChild(canvas);
    
    const ctx = canvas.getContext('2d');
    const dataArray = new Float32Array(analyser.frequencyBinCount);
    
    function draw() {
        requestAnimationFrame(draw);
        analyser.getFloatFrequencyData(dataArray);
        
        ctx.fillStyle = 'rgb(0, 0, 0)';
        ctx.fillRect(0, 0, canvas.width, canvas.height);
        ctx.lineWidth = 2;
        ctx.strokeStyle = 'rgb(0, 255, 0)';
        ctx.beginPath();
        
        const sliceWidth = canvas.width / dataArray.length;
        let x = 0;
        
        for (let i = 0; i < dataArray.length; i++) {
            const v = (dataArray[i] + 140) / 140; // normalize -140dB to 0dB
            const y = canvas.height - (v * canvas.height);
            
            if (i === 0) ctx.moveTo(x, y);
            else ctx.lineTo(x, y);
            
            x += sliceWidth;
        }
        
        ctx.stroke();
        
        // Draw threshold line
        ctx.strokeStyle = 'rgb(255, 0, 0)';
        ctx.beginPath();
        const thresholdY = canvas.height - ((-85 + 140) / 140 * canvas.height);
        ctx.moveTo(0, thresholdY);
        ctx.lineTo(canvas.width, thresholdY);
        ctx.stroke();
    }
    
    draw();
    console.log('Spectrum visualizer added. Red line = -85dB threshold.');
}
```

## 常见问题排查结果

| 症状 | 原因 | 解决方案 |
|------|------|---------|
| Console 显示 `Permission denied` | 麦克风权限未授予 | 在系统设置中允许麦克风访问 |
| FFT 电平始终低于 -85dB | 输入音量太小或设备错误 | 1) 提高音频接口增益<br>2) 切换到正确设备 |
| 设备下拉菜单显示 "Default" 但无其他选项 | `enumerateDevices` 权限问题 | 刷新页面并允许麦克风权限 |
| Audio Engine 显示频率但 Free Play 不工作 | Web Audio 和 JUCE 使用不同设备 | 在两处都选择同一设备 |
| MIDI Guitar 工作但 Slopsmith 不工作 | 设备独占或采样率不匹配 | 关闭 MIDI Guitar 再试 |

## 下一步

完成上述步骤后，请报告：
1. Console 中的错误信息
2. `webAudioOk` 的值
3. FFT 最大电平（dB）
4. 当前选择的设备名称
5. Audio Engine 面板的 Pitch Detection 是否有显示
