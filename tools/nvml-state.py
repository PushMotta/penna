"""What NVML says about each NVIDIA fan: policy, target, speed and RPM. Read-only without arguments, and needs no root.

With --default (root), hands every fan of every GPU back to the driver's own curve (nvmlDeviceSetDefaultFanSpeed_v2).
The M4 hardware test uses that only as a last resort in its cleanup, after `penna restore`.
Run with python3 -I.
"""
import ctypes as C
import json
import sys

POLICY = {0: "driver", 1: "manual"}


class FanSpeedInfo(C.Structure):
    _fields_ = [("version", C.c_uint), ("fan", C.c_uint), ("speed", C.c_uint)]


def main():
    nvml = C.CDLL("libnvidia-ml.so.1")
    nvml.nvmlErrorString.restype = C.c_char_p
    if nvml.nvmlInit_v2() != 0:
        print("NVML did not initialise", file=sys.stderr)
        return 2
    try:
        count = C.c_uint()
        nvml.nvmlDeviceGetCount_v2(C.byref(count))
        gpus = []
        for i in range(count.value):
            h = C.c_void_p()
            nvml.nvmlDeviceGetHandleByIndex_v2(i, C.byref(h))
            uuid = C.create_string_buffer(96)
            nvml.nvmlDeviceGetUUID(h, uuid, 96)
            temp = C.c_uint()
            nvml.nvmlDeviceGetTemperature(h, 0, C.byref(temp))
            nfans = C.c_uint()
            nvml.nvmlDeviceGetNumFans(h, C.byref(nfans))
            fans = []
            for f in range(nfans.value):
                if "--default" in sys.argv:
                    r = nvml.nvmlDeviceSetDefaultFanSpeed_v2(h, f)
                    if r != 0:
                        print(f"fan {f}: {nvml.nvmlErrorString(r).decode()}", file=sys.stderr)
                policy, target, speed = C.c_uint(), C.c_uint(), C.c_uint()
                nvml.nvmlDeviceGetFanControlPolicy_v2(h, f, C.byref(policy))
                nvml.nvmlDeviceGetTargetFanSpeed(h, f, C.byref(target))
                nvml.nvmlDeviceGetFanSpeed_v2(h, f, C.byref(speed))
                info = FanSpeedInfo(version=C.sizeof(FanSpeedInfo) | (1 << 24), fan=f)
                rpm = info.speed if nvml.nvmlDeviceGetFanSpeedRPM(h, C.byref(info)) == 0 else None
                fans.append({"fan": f, "policy": POLICY.get(policy.value, str(policy.value)), "target": target.value,
                             "speed": speed.value, "rpm": rpm})
            gpus.append({"uuid": uuid.value.decode(), "temp": temp.value, "fans": fans})
    finally:
        nvml.nvmlShutdown()
    if "--json" in sys.argv:
        print(json.dumps(gpus))
    else:
        for g in gpus:
            fans = "  ".join(f"fan{f['fan']} {f['policy']} target {f['target']}% speed {f['speed']}% {f['rpm']} RPM" for f in g["fans"])
            print(f"{g['uuid'][:12]}… {g['temp']} °C  {fans}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
