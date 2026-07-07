
cd /Users/mac/codes/slopSmith/slopsmith

# 运行所有测试
#pytest

# 运行特定文件
pytest tests/test_song.py -v

# 按模式匹配
pytest -k "round_trip" -v

cd /Users/mac/codes/slopSmith/slopsmith
PYTHONPATH=$PWD:$PWD/lib .venv/bin/uvicorn server:app --host 0.0.0.0 --port 8001





