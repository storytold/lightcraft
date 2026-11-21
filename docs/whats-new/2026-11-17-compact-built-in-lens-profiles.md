### Lens profiles for compact cameras that name no lens
- Raws from compact cameras that record no lens name, such as the Canon PowerShot G7 X or Sony RX100 II, now get
  their lens profile from the lens database (issue #3855). The lens is built in, so the camera is enough to find it.
- This changes how those photos render when Enable Lens Corrections is on: the profile's distortion and vignetting
  corrections now apply. A camera body with an unnamed lens still gets no profile, since any lens could have been
  on it.
