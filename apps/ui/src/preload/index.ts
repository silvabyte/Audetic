import { contextBridge } from "electron";

const audetic = {
  platform: process.platform,
};

try {
  contextBridge.exposeInMainWorld("audetic", audetic);
} catch (error) {
  console.error(error);
}

export type AudeticBridge = typeof audetic;
