export const stopChild = async (child) => {
  if (!child || child.pid === undefined || child.exitCode !== null || child.signalCode !== null) return;
  await new Promise((resolve) => {
    const timeout = setTimeout(() => child.kill("SIGKILL"), 2_000);
    child.once("close", () => { clearTimeout(timeout); resolve(); });
    child.kill("SIGTERM");
  });
};
