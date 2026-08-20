// 在 Console 粘贴此代码来诊断 Free Play 检测问题
// 显示实时检测到的音符和FFT能量分布

(function() {
    console.clear();
    console.log('=== Free Play 诊断工具 ===');
    console.log('拨弦后观察输出\n');

    const debug = window.freeplay?.__debug;
    if (!debug) {
        console.error('❌ window.freeplay.__debug 不存在');
        console.log('请确保：');
        console.log('  1. 已进入 Free Play 模式');
        console.log('  2. 已用最新代码重启应用');
        return;
    }

    const analyser = debug.analyser;
    if (!analyser) {
        console.error('❌ analyser 未初始化');
        console.log('可能原因：');
        console.log('  1. 麦克风权限被拒绝');
        console.log('  2. Web Audio 初始化失败');
        return;
    }

    console.log('✓ analyser 已就绪');
    console.log('  Sample rate:', debug.sampleRate, 'Hz');
    console.log('  FFT size:', analyser.fftSize);
    console.log('  Bin resolution:', (debug.sampleRate / analyser.fftSize).toFixed(2), 'Hz/bin');
    console.log('');

    // 吉他各弦的频率
    const strings = [
        { name: '6弦(E2)', freq: 82.41, midi: 40 },
        { name: '5弦(A2)', freq: 110.00, midi: 45 },
        { name: '4弦(D3)', freq: 146.83, midi: 50 },
        { name: '3弦(G3)', freq: 196.00, midi: 55 },
        { name: '2弦(B3)', freq: 246.94, midi: 59 },
        { name: '1弦(E4)', freq: 329.63, midi: 64 }
    ];

    const freqData = new Float32Array(analyser.frequencyBinCount);
    const binHz = debug.sampleRate / analyser.fftSize;

    let frameCount = 0;
    const CHECK_INTERVAL = 500; // ms

    const checker = setInterval(() => {
        analyser.getFloatFrequencyData(freqData);

        // 找出最强的5个频率分量
        const peaks = [];
        for (let i = 1; i < freqData.length - 1; i++) {
            if (freqData[i] > -85 &&
                freqData[i] > freqData[i-1] &&
                freqData[i] > freqData[i+1]) {
                peaks.push({ bin: i, freq: i * binHz, db: freqData[i] });
            }
        }
        peaks.sort((a, b) => b.db - a.db);
        const top5 = peaks.slice(0, 5);

        if (top5.length === 0) {
            // 静音
            return;
        }

        frameCount++;
        console.log(`\n--- Frame ${frameCount} ---`);
        console.log('检测到的频率峰值（前5）：');
        top5.forEach((p, i) => {
            // 找最接近的音符
            const midi = 69 + 12 * Math.log2(p.freq / 440);
            const midiRound = Math.round(midi);
            const noteName = ['C','C#','D','D#','E','F','F#','G','G#','A','A#','B'][midiRound % 12];
            const octave = Math.floor(midiRound / 12) - 1;
            console.log(`  ${i+1}. ${p.freq.toFixed(1)} Hz = ${noteName}${octave} (MIDI ${midiRound}) @ ${p.db.toFixed(1)} dB`);
        });

        // 检查各弦的基音能量
        console.log('\n各弦基音能量：');
        strings.forEach(s => {
            const bin = Math.round(s.freq / binHz);
            const db = freqData[bin];
            const status = db > -85 ? '✓' : '✗';
            console.log(`  ${status} ${s.name}: ${db.toFixed(1)} dB ${db > -85 ? '(可检测)' : '(低于阈值)'}`);
        });

        // 检查当前激活的音符
        const active = debug.activeNotes;
        if (active && active.length > 0) {
            console.log('\n当前激活音符：');
            active.forEach(([key, note]) => {
                const sName = ['6弦','5弦','4弦','3弦','2弦','1弦'][note.s];
                console.log(`  ${sName} 品${note.f} (MIDI ${note.midi})`);
            });
        }

    }, CHECK_INTERVAL);

    console.log('监控已启动，每', CHECK_INTERVAL, 'ms 更新一次');
    console.log('停止监控：clearInterval(' + checker + ')');

    window._fpDiag = checker;
})();
