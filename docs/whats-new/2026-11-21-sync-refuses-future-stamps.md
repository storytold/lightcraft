### Device sync holds back changes from the future

- A change from another device stamped more than five minutes ahead of this device's clock is held back instead of taken, and a notice names the device whose clock is wrong. Only the fields dated in the future wait: the rest of the same photo's changes arrive at once. Held changes are kept and tried again on each exchange, so nothing is lost. Before, one device with a clock set days ahead could make its edits win over every later edit on every device.
