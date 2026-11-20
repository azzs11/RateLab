# 🎮 Demo Guide

## Quick Start

### 1. Start the Server
```bash
cargo run -p server --bin web-server
```

### 2. Open Dashboard

Navigate to: http://localhost:3001

---

## 🎯 Demo Scenarios

### Scenario 1: Basic Rate Limiting

**Goal**: Show how rate limiting blocks excessive requests

1. Set **Max Requests: 10**
2. Set **Test Requests: 20**
3. Click **Run Test**
4. **Result**: First 10 allowed ✅, next 10 blocked ❌

### Scenario 2: Algorithm Comparison

**Goal**: Compare Fixed Window (simple counter) vs Sliding Window (exact timestamps)

1. Test with **Fixed Window**: note the allowed/blocked counts and the throughput the dashboard reports
2. Test with **Sliding Window** with the same settings
3. **Result**: compare the numbers your run produced. They depend on your machine, so this guide does not quote any.

### Scenario 3: Burst Handling

**Goal**: Show Token Bucket allows bursts

1. Algorithm: **Token Bucket**
2. Max Requests: **50**, Window: **1 second**
3. Test Requests: **100**
4. **Result**: roughly the bucket's capacity is allowed in one burst, the rest blocked

### Scenario 4: Repeated Runs

**Goal**: See how the measured duration varies between runs

1. Max Requests: **100**
2. Test Requests: **200**
3. Run multiple times
4. **Result**: compare the duration and throughput the dashboard reports for each run

---

## 🎥 Recording a Demo

### 30-Second Demo Script:

1. **0:00-0:05**: Show dashboard, explain what it does
2. **0:05-0:10**: Adjust sliders, show configurability
3. **0:10-0:15**: Click "Run Test", watch results appear
4. **0:15-0:20**: Show green/red dot visualization
5. **0:20-0:25**: Switch algorithm, run again
6. **0:25-0:30**: Show comparison page

### Recording Tips:

- Use OBS Studio or Windows Game Bar
- 1920x1080 resolution
- Speak clearly and enthusiastically
- Keep it under 1 minute
- Upload to YouTube, add to README