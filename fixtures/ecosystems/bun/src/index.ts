import ms from "ms";

export function seconds(text: string): number {
  return Number(ms(text)) / 1000;
}
