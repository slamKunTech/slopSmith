# 在运行构建之前，先启动代理：
  source ./Users/mac/proxy-start.sh

  # 然后正常运行构建：
  bash mycompileExe.sh

  # 构建完成后可以关掉代理：
  source ./Users/mac/proxy-stop.sh
  
  代理脚本用了你给的服务器信息：                                              
  - ss-local → socks5://127.0.0.1:1080
  - 服务器 47.86.215.76:8388，chacha20-ietf-poly1305 加密



