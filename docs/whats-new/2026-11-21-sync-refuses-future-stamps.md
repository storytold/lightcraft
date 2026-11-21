### Device sync refuses changes from the future

- A change from another device stamped more than five minutes ahead of this device's clock is refused instead of taken, and the log names the device. Before, one device with a clock set days ahead could make its edits win over every later edit on every device.
