/** Safe delivery defaults; opting in must be explicit. */
export const DEFAULT_CAPTURE_OPTIONS = {
  title: null,
  capture_source: "microphone",
  review_before_processing: false,
  auto_paste: false,
  copy_to_clipboard: false,
} as const;
