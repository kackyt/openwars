/** Google Analytics の計測 ID。未設定なら undefined（計測なしで動作する）。 */
export function gaMeasurementId(): string | undefined {
  const id = import.meta.env.VITE_GA_MEASUREMENT_ID;
  return typeof id === "string" && id.trim().length > 0 ? id : undefined;
}
