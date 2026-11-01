const captureTransitionTrace: string[] = [];
const CAPTURE_TRACE_LIMIT = 64;

export function recordCaptureTransitionForTests(eventType: string): void {
  captureTransitionTrace.push(eventType);
  if (captureTransitionTrace.length > CAPTURE_TRACE_LIMIT) {
    captureTransitionTrace.splice(0, captureTransitionTrace.length - CAPTURE_TRACE_LIMIT);
  }
}

export function readCaptureTransitionTrace(): readonly string[] {
  return captureTransitionTrace;
}

export function resetCaptureTransitionTrace(): void {
  captureTransitionTrace.length = 0;
}
